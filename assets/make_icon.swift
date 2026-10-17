// Renders the DevDisk app icon (a small treemap on a dark squircle) to a 1024px PNG.
// Usage: swift assets/make_icon.swift assets/icon_1024.png
import CoreGraphics
import Foundation
import ImageIO

let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon_1024.png"
let S: CGFloat = 1024
let cs = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: Int(S), height: Int(S), bitsPerComponent: 8, bytesPerRow: 0,
                    space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: a)
}

func vgradient(_ rect: CGRect, _ top: CGColor, _ bottom: CGColor) {
    let g = CGGradient(colorsSpace: cs, colors: [bottom, top] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: rect.midX, y: rect.minY), end: CGPoint(x: rect.midX, y: rect.maxY), options: [])
}

// Squircle body (macOS icon grid: 824pt body inside the 1024 canvas).
let body = CGRect(x: 100, y: 100, width: 824, height: 824)
let bodyPath = CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil)

ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: rgb(0x000000, 0.45))
ctx.addPath(bodyPath)
ctx.setFillColor(rgb(0x141418))
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(bodyPath)
ctx.clip()
vgradient(body, rgb(0x26262e), rgb(0x0f0f13))

// Treemap tiles, in unit coordinates of the inner area (y down), with gaps.
let inner = CGRect(x: 196, y: 196, width: 632, height: 632)
let gap: CGFloat = 24
let tiles: [(CGFloat, CGFloat, CGFloat, CGFloat, UInt32, UInt32)] = [
    (0.00, 0.00, 0.56, 1.00, 0x8b9bff, 0x5b64dc), // big folder: indigo
    (0.56, 0.00, 1.00, 0.48, 0xae8ff2, 0x7c5bd2), // violet
    (0.56, 0.48, 0.80, 1.00, 0xffbe5c, 0xe8862c), // dev junk: amber
    (0.80, 0.48, 1.00, 0.74, 0x5fd3c3, 0x2f9e95), // teal
    (0.80, 0.74, 1.00, 1.00, 0x5c6178, 0x3e4256), // slate
]
for (x0, y0, x1, y1, top, bottom) in tiles {
    // Flip y: unit y-down -> CG y-up.
    var r = CGRect(x: inner.minX + x0 * inner.width, y: inner.maxY - y1 * inner.height,
                   width: (x1 - x0) * inner.width, height: (y1 - y0) * inner.height)
    r = r.insetBy(dx: gap / 2, dy: gap / 2)
    let p = CGPath(roundedRect: r, cornerWidth: 40, cornerHeight: 40, transform: nil)
    ctx.saveGState()
    ctx.addPath(p)
    ctx.clip()
    vgradient(r, rgb(top), rgb(bottom))
    // Soft top sheen on each tile.
    let sheen = CGRect(x: r.minX, y: r.maxY - r.height * 0.45, width: r.width, height: r.height * 0.45)
    vgradient(sheen, rgb(0xffffff, 0.16), rgb(0xffffff, 0.0))
    ctx.restoreGState()
}

// Glassy highlight across the top half of the body.
let gloss = CGRect(x: body.minX, y: body.midY, width: body.width, height: body.height / 2)
vgradient(gloss, rgb(0xffffff, 0.07), rgb(0xffffff, 0.0))
ctx.restoreGState()

// Hairline inner edge.
ctx.addPath(CGPath(roundedRect: body.insetBy(dx: 1.5, dy: 1.5), cornerWidth: 184, cornerHeight: 184, transform: nil))
ctx.setStrokeColor(rgb(0xffffff, 0.10))
ctx.setLineWidth(3)
ctx.strokePath()

let img = ctx.makeImage()!
let url = URL(fileURLWithPath: out) as CFURL
let dest = CGImageDestinationCreateWithURL(url, "public.png" as CFString, 1, nil)!
CGImageDestinationAddImage(dest, img, nil)
CGImageDestinationFinalize(dest)
print("wrote \(out)")
