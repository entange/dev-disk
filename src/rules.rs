//! Dev / AI junk detection rules.
//!
//! Design goal: false negatives are fine, false positives are NOT.
//! Every name-based rule requires corroborating marker files (e.g. a
//! `node_modules` is only flagged when its parent has a `package.json`),
//! and name-based rules only apply inside the "project zone": the user's
//! home folder excluding `~/Library`, `~/Applications`, hidden top-level
//! folders (`~/.vscode`, `~/.nvm`, ...) and anything inside app bundles.
//! Fixed-location caches (Xcode DerivedData, Homebrew cache, ...) are
//! matched by exact path only.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const NO_RULE: u16 = u16::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Category {
    JavaScript,
    Rust,
    Python,
    Apple,
    Jvm,
    Go,
    Dart,
    OtherLangs,
    PackageManagers,
    AiModels,
    Editors,
    Containers,
    GenericCache,
}

impl Category {
    pub const ALL: [Category; 13] = [
        Category::JavaScript,
        Category::Rust,
        Category::Python,
        Category::Apple,
        Category::Jvm,
        Category::Go,
        Category::Dart,
        Category::OtherLangs,
        Category::PackageManagers,
        Category::AiModels,
        Category::Editors,
        Category::Containers,
        Category::GenericCache,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::JavaScript => "JavaScript / Node",
            Category::Rust => "Rust",
            Category::Python => "Python",
            Category::Apple => "Xcode / Swift / CocoaPods",
            Category::Jvm => "Java / Kotlin / Android",
            Category::Go => "Go",
            Category::Dart => "Flutter / Dart",
            Category::OtherLangs => "Other languages",
            Category::PackageManagers => "Homebrew & package managers",
            Category::AiModels => "AI / ML models",
            Category::Editors => "Editors & IDE caches",
            Category::Containers => "Containers & VMs",
            Category::GenericCache => "Other tagged caches",
        }
    }

    /// Hue (0..1) used for badges.
    pub fn hue(self) -> f32 {
        match self {
            Category::JavaScript => 0.14,
            Category::Rust => 0.05,
            Category::Python => 0.58,
            Category::Apple => 0.62,
            Category::Jvm => 0.02,
            Category::Go => 0.52,
            Category::Dart => 0.55,
            Category::OtherLangs => 0.75,
            Category::PackageManagers => 0.09,
            Category::AiModels => 0.83,
            Category::Editors => 0.60,
            Category::Containers => 0.50,
            Category::GenericCache => 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Risk {
    /// Regenerated automatically by the tool (build output, caches).
    Safe,
    /// Recreatable but costs a re-download / re-install, or has a caveat.
    Caution,
    /// Shown for visibility only. The app will never trash these.
    InfoOnly,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "Safe",
            Risk::Caution => "Review",
            Risk::InfoOnly => "Info only",
        }
    }
}

pub enum Matcher {
    /// Exact path relative to the home folder.
    Home(&'static str),
    /// Directory with one of these names. `parent` / `child` are marker
    /// masks: when non-zero, at least one marker of the mask must exist in
    /// the parent directory / inside the directory itself.
    Name {
        names: &'static [&'static str],
        parent: u64,
        child: u64,
    },
    /// Any directory name, identified purely by its contents.
    AnyName { child: u64 },
}

pub struct Rule {
    pub label: &'static str,
    pub category: Category,
    pub risk: Risk,
    pub note: &'static str,
    pub matcher: Matcher,
}

// ---------------------------------------------------------------- markers

pub const M_PACKAGE_JSON: u64 = 1 << 0;
pub const M_TURBO_JSON: u64 = 1 << 1;
pub const M_CARGO: u64 = 1 << 2;
pub const M_RUSTC_INFO: u64 = 1 << 3;
pub const M_CACHEDIR: u64 = 1 << 4;
pub const M_POM: u64 = 1 << 5;
pub const M_MAVEN_OUT: u64 = 1 << 6;
pub const M_CLASSES: u64 = 1 << 7;
pub const M_GRADLE: u64 = 1 << 8;
pub const M_GRADLE_OUT: u64 = 1 << 9;
pub const M_PUBSPEC: u64 = 1 << 10;
pub const M_FLUTTER_OUT: u64 = 1 << 11;
pub const M_SWIFTPM: u64 = 1 << 12;
pub const M_SWIFTPM_OUT: u64 = 1 << 13;
pub const M_PODFILE: u64 = 1 << 14;
pub const M_PODS_MANIFEST: u64 = 1 << 15;
pub const M_MIX: u64 = 1 << 16;
pub const M_PYVENV: u64 = 1 << 17;
pub const M_PYPROJECT: u64 = 1 << 18;
pub const M_STACK: u64 = 1 << 19;
pub const M_ZIG: u64 = 1 << 20;
pub const M_COMPOSER: u64 = 1 << 21;
pub const M_AUTOLOAD_PHP: u64 = 1 << 22;
pub const M_TF_OUT: u64 = 1 << 23;
pub const M_NEXT_OUT: u64 = 1 << 24;
pub const M_NUXT_CONFIG: u64 = 1 << 25;
pub const M_ANGULAR: u64 = 1 << 26;
pub const M_COVERAGE_OUT: u64 = 1 << 27;

/// Single source of truth for marker names, used both while scanning and
/// when re-verifying right before anything is moved to the Trash.
const MARKER_NAMES: &[(&str, u64)] = &[
    ("package.json", M_PACKAGE_JSON),
    ("turbo.json", M_TURBO_JSON),
    ("Cargo.toml", M_CARGO),
    (".rustc_info.json", M_RUSTC_INFO),
    ("CACHEDIR.TAG", M_CACHEDIR),
    ("pom.xml", M_POM),
    ("maven-status", M_MAVEN_OUT),
    ("maven-archiver", M_MAVEN_OUT),
    ("classes", M_CLASSES),
    ("build.gradle", M_GRADLE),
    ("build.gradle.kts", M_GRADLE),
    ("settings.gradle", M_GRADLE),
    ("settings.gradle.kts", M_GRADLE),
    ("tmp", M_GRADLE_OUT),
    ("intermediates", M_GRADLE_OUT),
    ("outputs", M_GRADLE_OUT),
    ("generated", M_GRADLE_OUT),
    ("reports", M_GRADLE_OUT),
    ("pubspec.yaml", M_PUBSPEC),
    (".last_build_id", M_FLUTTER_OUT),
    ("flutter_assets", M_FLUTTER_OUT),
    ("native_assets", M_FLUTTER_OUT),
    ("Package.swift", M_SWIFTPM),
    ("workspace-state.json", M_SWIFTPM_OUT),
    ("checkouts", M_SWIFTPM_OUT),
    ("repositories", M_SWIFTPM_OUT),
    ("debug", M_SWIFTPM_OUT),
    ("release", M_SWIFTPM_OUT),
    ("Podfile", M_PODFILE),
    ("Manifest.lock", M_PODS_MANIFEST),
    ("mix.exs", M_MIX),
    ("pyvenv.cfg", M_PYVENV),
    ("pyproject.toml", M_PYPROJECT),
    ("setup.py", M_PYPROJECT),
    ("setup.cfg", M_PYPROJECT),
    ("tox.ini", M_PYPROJECT),
    ("noxfile.py", M_PYPROJECT),
    ("requirements.txt", M_PYPROJECT),
    ("Pipfile", M_PYPROJECT),
    ("poetry.lock", M_PYPROJECT),
    ("uv.lock", M_PYPROJECT),
    ("stack.yaml", M_STACK),
    ("build.zig", M_ZIG),
    ("composer.json", M_COMPOSER),
    ("autoload.php", M_AUTOLOAD_PHP),
    ("providers", M_TF_OUT),
    ("modules", M_TF_OUT),
    ("BUILD_ID", M_NEXT_OUT),
    ("build-manifest.json", M_NEXT_OUT),
    ("server", M_NEXT_OUT),
    ("static", M_NEXT_OUT),
    ("cache", M_NEXT_OUT),
    ("trace", M_NEXT_OUT),
    ("types", M_NEXT_OUT),
    ("nuxt.config.ts", M_NUXT_CONFIG),
    ("nuxt.config.js", M_NUXT_CONFIG),
    ("nuxt.config.mjs", M_NUXT_CONFIG),
    ("angular.json", M_ANGULAR),
    ("lcov.info", M_COVERAGE_OUT),
    ("lcov-report", M_COVERAGE_OUT),
    ("coverage-final.json", M_COVERAGE_OUT),
    ("coverage-summary.json", M_COVERAGE_OUT),
    ("clover.xml", M_COVERAGE_OUT),
];

fn marker_map() -> &'static HashMap<&'static [u8], u64> {
    static MAP: OnceLock<HashMap<&'static [u8], u64>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<&'static [u8], u64> = HashMap::new();
        for (name, bit) in MARKER_NAMES {
            *m.entry(name.as_bytes()).or_insert(0) |= *bit;
        }
        m
    })
}

/// Marker bits for a directory entry name (called for every entry while scanning).
#[inline]
pub fn marker_for(name: &[u8]) -> u64 {
    if name.len() > 22 || name.len() < 3 {
        return 0;
    }
    marker_map().get(name).copied().unwrap_or(0)
}

/// Re-check on disk that a marker of `mask` exists in `dir`.
fn dir_has_marker(dir: &Path, mask: u64) -> bool {
    MARKER_NAMES
        .iter()
        .filter(|(_, bit)| bit & mask != 0)
        .any(|(name, _)| std::fs::symlink_metadata(dir.join(name)).is_ok())
}

// ------------------------------------------------------------------ rules

use Category::*;
use Risk::*;

macro_rules! home {
    ($label:expr, $cat:expr, $risk:expr, $note:expr, $path:expr) => {
        Rule { label: $label, category: $cat, risk: $risk, note: $note, matcher: Matcher::Home($path) }
    };
}
macro_rules! named {
    ($label:expr, $cat:expr, $risk:expr, $note:expr, $names:expr, $parent:expr, $child:expr) => {
        Rule {
            label: $label,
            category: $cat,
            risk: $risk,
            note: $note,
            matcher: Matcher::Name { names: $names, parent: $parent, child: $child },
        }
    };
}

pub static RULES: &[Rule] = &[
    // ---- JavaScript / Node (project level)
    named!("node_modules", JavaScript, Safe, "Restored by npm / pnpm / yarn / bun install", &["node_modules"], M_PACKAGE_JSON, 0),
    named!("Next.js build & cache (.next)", JavaScript, Safe, "Rebuilt by next dev / next build", &[".next"], M_PACKAGE_JSON, M_NEXT_OUT),
    named!("Turborepo cache (.turbo)", JavaScript, Safe, "Rebuilt by turbo", &[".turbo"], M_PACKAGE_JSON | M_TURBO_JSON, 0),
    named!("Nuxt build output", JavaScript, Safe, "Rebuilt by nuxt dev / build", &[".nuxt", ".output"], M_NUXT_CONFIG, 0),
    named!("SvelteKit output (.svelte-kit)", JavaScript, Safe, "Regenerated by vite / svelte-kit sync", &[".svelte-kit"], M_PACKAGE_JSON, 0),
    named!("Parcel cache", JavaScript, Safe, "Rebuilt by parcel", &[".parcel-cache"], M_PACKAGE_JSON, 0),
    named!("Angular CLI cache", JavaScript, Safe, "Rebuilt by ng build", &[".angular"], M_ANGULAR, 0),
    named!("Docusaurus cache", JavaScript, Safe, "Rebuilt by docusaurus", &[".docusaurus"], M_PACKAGE_JSON, 0),
    named!("Astro generated (.astro)", JavaScript, Safe, "Regenerated by astro", &[".astro"], M_PACKAGE_JSON, 0),
    named!("Expo local cache (.expo)", JavaScript, Safe, "Regenerated by expo start", &[".expo"], M_PACKAGE_JSON, 0),
    named!("Test coverage report", JavaScript, Safe, "Regenerated by your test runner", &["coverage"], M_PACKAGE_JSON, M_COVERAGE_OUT),
    // ---- JavaScript (global caches)
    home!("npm cache", JavaScript, Safe, "Same as `npm cache clean --force`", ".npm/_cacache"),
    home!("npx cache", JavaScript, Safe, "Re-downloaded on next npx run", ".npm/_npx"),
    home!("Yarn cache", JavaScript, Safe, "Same as `yarn cache clean`", "Library/Caches/Yarn"),
    home!("Yarn Berry cache", JavaScript, Safe, "Re-downloaded on install", ".yarn/berry/cache"),
    home!("pnpm store", JavaScript, Caution, "Prefer `pnpm store prune`. Files hard-linked into projects are not freed", "Library/pnpm/store"),
    home!("pnpm store", JavaScript, Caution, "Prefer `pnpm store prune`. Files hard-linked into projects are not freed", ".pnpm-store"),
    home!("Bun install cache", JavaScript, Safe, "Re-downloaded on install", ".bun/install/cache"),
    home!("Playwright browsers", JavaScript, Caution, "Re-download with `npx playwright install`", "Library/Caches/ms-playwright"),
    home!("Cypress binaries", JavaScript, Caution, "Re-downloaded on `cypress install`", "Library/Caches/Cypress"),
    home!("Puppeteer browsers", JavaScript, Caution, "Re-downloaded on install", ".cache/puppeteer"),
    home!("node-gyp headers", JavaScript, Safe, "Re-downloaded when native modules build", "Library/Caches/node-gyp"),
    home!("node-gyp headers", JavaScript, Safe, "Re-downloaded when native modules build", ".node-gyp"),
    home!("Corepack cache", JavaScript, Safe, "Re-downloaded by corepack", ".cache/node/corepack"),
    home!("TypeScript typings cache", JavaScript, Safe, "Re-downloaded by the TS server", "Library/Caches/typescript"),
    // ---- Rust
    named!("Rust build output (target)", Rust, Safe, "Rebuilt by cargo build", &["target"], M_CARGO, M_RUSTC_INFO | M_CACHEDIR),
    home!("Cargo crate archives", Rust, Safe, "Re-downloaded by cargo", ".cargo/registry/cache"),
    home!("Cargo extracted sources", Rust, Safe, "Re-extracted by cargo", ".cargo/registry/src"),
    home!("Cargo git checkouts", Rust, Safe, "Re-checked-out by cargo", ".cargo/git/checkouts"),
    home!("Rust toolchains", Rust, InfoOnly, "Use `rustup toolchain list` / `rustup toolchain uninstall`", ".rustup/toolchains"),
    // ---- Python
    named!("__pycache__", Python, Safe, "Regenerated by Python", &["__pycache__"], 0, 0),
    named!("Python tool cache", Python, Safe, "pytest / mypy / ruff / hypothesis caches", &[".pytest_cache", ".mypy_cache", ".ruff_cache", ".hypothesis"], 0, 0),
    named!("tox / nox envs", Python, Safe, "Recreated by tox / nox", &[".tox", ".nox"], M_PYPROJECT, 0),
    home!("pip cache", Python, Safe, "Same as `pip cache purge`", "Library/Caches/pip"),
    home!("pip cache", Python, Safe, "Same as `pip cache purge`", ".cache/pip"),
    home!("uv cache", Python, Safe, "Same as `uv cache clean`", ".cache/uv"),
    home!("Poetry cache", Python, Safe, "Re-downloaded by poetry", "Library/Caches/pypoetry"),
    home!("pre-commit envs", Python, Safe, "Recreated by pre-commit", ".cache/pre-commit"),
    home!("conda packages", Python, Caution, "Prefer `conda clean --all`", ".conda/pkgs"),
    home!("conda packages", Python, Caution, "Prefer `conda clean --all`", "miniconda3/pkgs"),
    home!("conda packages", Python, Caution, "Prefer `conda clean --all`", "anaconda3/pkgs"),
    home!("conda packages", Python, Caution, "Prefer `conda clean --all`", "miniforge3/pkgs"),
    // ---- Apple
    home!("Xcode DerivedData", Apple, Safe, "Rebuilt by Xcode", "Library/Developer/Xcode/DerivedData"),
    home!("iOS DeviceSupport", Apple, Safe, "Re-created when a device connects", "Library/Developer/Xcode/iOS DeviceSupport"),
    home!("watchOS DeviceSupport", Apple, Safe, "Re-created when a device connects", "Library/Developer/Xcode/watchOS DeviceSupport"),
    home!("SwiftUI previews", Apple, Safe, "Rebuilt by Xcode", "Library/Developer/Xcode/UserData/Previews"),
    home!("Xcode caches", Apple, Safe, "Rebuilt by Xcode", "Library/Caches/com.apple.dt.Xcode"),
    home!("Simulator caches", Apple, Safe, "Rebuilt by the simulator", "Library/Developer/CoreSimulator/Caches"),
    home!("Xcode Archives", Apple, Caution, "Holds dSYMs needed to symbolicate crash reports of shipped builds", "Library/Developer/Xcode/Archives"),
    home!("Simulator devices", Apple, InfoOnly, "Use `xcrun simctl delete unavailable` or Xcode > Devices", "Library/Developer/CoreSimulator/Devices"),
    home!("CocoaPods cache", Apple, Safe, "Re-downloaded by pod install", "Library/Caches/CocoaPods"),
    home!("CocoaPods spec repos", Apple, Caution, "Re-fetched by `pod repo update`", ".cocoapods/repos"),
    home!("Carthage cache", Apple, Safe, "Re-downloaded by carthage", "Library/Caches/org.carthage.CarthageKit"),
    named!("SwiftPM build (.build)", Apple, Safe, "Rebuilt by swift build", &[".build"], M_SWIFTPM, M_SWIFTPM_OUT),
    named!("CocoaPods Pods", Apple, Caution, "Restored by `pod install`", &["Pods"], M_PODFILE, M_PODS_MANIFEST),
    // ---- JVM / Android
    named!("Gradle build output", Jvm, Safe, "Rebuilt by gradle", &["build"], M_GRADLE, M_GRADLE_OUT | M_CLASSES),
    named!("Gradle project cache (.gradle)", Jvm, Safe, "Rebuilt by gradle", &[".gradle"], M_GRADLE, 0),
    named!("Kotlin / NDK build cache", Jvm, Safe, "Rebuilt by gradle", &[".kotlin", ".cxx"], M_GRADLE, 0),
    named!("Maven build output (target)", Jvm, Safe, "Rebuilt by mvn package", &["target"], M_POM, M_MAVEN_OUT | M_CLASSES),
    home!("Gradle caches", Jvm, Safe, "Re-downloaded by gradle", ".gradle/caches"),
    home!("Gradle wrapper dists", Jvm, Safe, "Re-downloaded by ./gradlew", ".gradle/wrapper/dists"),
    home!("Gradle daemon logs", Jvm, Safe, "Logs only", ".gradle/daemon"),
    home!("Maven repository", Jvm, Caution, "Re-downloaded on next build", ".m2/repository"),
    home!("Android cache", Jvm, Safe, "Rebuilt by Android tools", ".android/cache"),
    home!("Android emulators", Jvm, InfoOnly, "Delete via Android Studio > Device Manager", ".android/avd"),
    home!("Android SDK", Jvm, InfoOnly, "Manage via Android Studio > SDK Manager", "Library/Android/sdk"),
    // ---- Go
    home!("Go build cache", Go, Safe, "Same as `go clean -cache`", "Library/Caches/go-build"),
    home!("Go module cache", Go, Caution, "Prefer `go clean -modcache`", "go/pkg/mod"),
    home!("golangci-lint cache", Go, Safe, "Rebuilt by golangci-lint", "Library/Caches/golangci-lint"),
    // ---- Dart / Flutter
    named!("Dart tool cache (.dart_tool)", Dart, Safe, "Rebuilt by flutter pub get", &[".dart_tool"], M_PUBSPEC, 0),
    named!("Flutter build output", Dart, Safe, "Rebuilt by flutter build", &["build"], M_PUBSPEC, M_FLUTTER_OUT),
    home!("Pub cache", Dart, Caution, "Re-downloaded by pub get", ".pub-cache"),
    // ---- Other languages
    named!("Elixir build (_build)", OtherLangs, Safe, "Rebuilt by mix compile", &["_build"], M_MIX, 0),
    named!("Haskell Stack work dir", OtherLangs, Safe, "Rebuilt by stack build", &[".stack-work"], M_STACK, 0),
    named!("Cabal build (dist-newstyle)", OtherLangs, Safe, "Rebuilt by cabal build", &["dist-newstyle"], 0, 0),
    named!("Zig cache / output", OtherLangs, Safe, "Rebuilt by zig build", &[".zig-cache", "zig-cache", "zig-out"], M_ZIG, 0),
    named!("Terraform providers (.terraform)", OtherLangs, Caution, "Re-downloaded by terraform init", &[".terraform"], 0, M_TF_OUT),
    named!("PHP Composer vendor", OtherLangs, Caution, "Restored by composer install", &["vendor"], M_COMPOSER, M_AUTOLOAD_PHP),
    home!("Deno cache", OtherLangs, Safe, "Re-downloaded by deno", "Library/Caches/deno"),
    home!("Zig global cache", OtherLangs, Safe, "Rebuilt by zig", ".cache/zig"),
    home!("NuGet packages", OtherLangs, Caution, "Re-downloaded by dotnet restore", ".nuget/packages"),
    // ---- Package managers
    home!("Homebrew downloads cache", PackageManagers, Safe, "Same as `brew cleanup -s`", "Library/Caches/Homebrew"),
    home!("Homebrew logs", PackageManagers, Safe, "Logs only", "Library/Logs/Homebrew"),
    // ---- AI / ML
    home!("Hugging Face models (hub)", AiModels, Caution, "Re-downloaded when next used. Your HF login token is not touched", ".cache/huggingface/hub"),
    home!("Hugging Face datasets", AiModels, Caution, "Re-downloaded when next used", ".cache/huggingface/datasets"),
    home!("Hugging Face xet cache", AiModels, Safe, "Download chunk cache", ".cache/huggingface/xet"),
    home!("Ollama models", AiModels, Caution, "Prefer `ollama rm <model>`. Models must be re-pulled", ".ollama/models"),
    home!("PyTorch hub cache", AiModels, Caution, "Checkpoints are re-downloaded when next used", ".cache/torch"),
    home!("Whisper models", AiModels, Caution, "Re-downloaded when next used", ".cache/whisper"),
    home!("LM Studio models", AiModels, Caution, "Must be re-downloaded in LM Studio", ".lmstudio/models"),
    home!("LM Studio models", AiModels, Caution, "Must be re-downloaded in LM Studio", ".cache/lm-studio/models"),
    home!("ModelScope cache", AiModels, Caution, "Re-downloaded when next used", ".cache/modelscope"),
    home!("KaggleHub cache", AiModels, Caution, "Re-downloaded when next used", ".cache/kagglehub"),
    home!("Keras datasets", AiModels, Caution, "Re-downloaded when next used", ".keras/datasets"),
    home!("Keras models", AiModels, Caution, "Re-downloaded when next used", ".keras/models"),
    // ---- Editors
    home!("VS Code cache", Editors, Safe, "Rebuilt by VS Code", "Library/Application Support/Code/Cache"),
    home!("VS Code cached data", Editors, Safe, "Rebuilt by VS Code", "Library/Application Support/Code/CachedData"),
    home!("VS Code extension downloads", Editors, Safe, "Installer copies of extensions", "Library/Application Support/Code/CachedExtensionVSIXs"),
    home!("VS Code logs", Editors, Safe, "Logs only", "Library/Application Support/Code/logs"),
    home!("VS Code updater cache", Editors, Safe, "Updater download", "Library/Caches/com.microsoft.VSCode.ShipIt"),
    home!("Cursor cache", Editors, Safe, "Rebuilt by Cursor", "Library/Application Support/Cursor/Cache"),
    home!("Cursor cached data", Editors, Safe, "Rebuilt by Cursor", "Library/Application Support/Cursor/CachedData"),
    home!("Cursor extension downloads", Editors, Safe, "Installer copies of extensions", "Library/Application Support/Cursor/CachedExtensionVSIXs"),
    home!("Cursor logs", Editors, Safe, "Logs only", "Library/Application Support/Cursor/logs"),
    home!("Windsurf cache", Editors, Safe, "Rebuilt by Windsurf", "Library/Application Support/Windsurf/Cache"),
    home!("Windsurf cached data", Editors, Safe, "Rebuilt by Windsurf", "Library/Application Support/Windsurf/CachedData"),
    home!("JetBrains caches", Editors, Safe, "IDE indexes are rebuilt", "Library/Caches/JetBrains"),
    home!("Zed cache", Editors, Safe, "Rebuilt by Zed", "Library/Caches/dev.zed.Zed"),
    // ---- Containers / VMs (never trashed by this app)
    home!("Docker Desktop disk image", Containers, InfoOnly, "Run `docker system prune -a` or shrink the disk limit in Docker settings", "Library/Containers/com.docker.docker/Data/vms"),
    home!("OrbStack data", Containers, InfoOnly, "Manage from OrbStack (`orb prune`)", ".orbstack"),
    home!("Colima VMs", Containers, InfoOnly, "Use `colima delete`", ".colima"),
    // ---- Generic, contents-identified (keep last)
    Rule {
        label: "Python virtualenv",
        category: Python,
        risk: Caution,
        note: "Recreate with python -m venv / uv sync / poetry install",
        matcher: Matcher::AnyName { child: M_PYVENV },
    },
    Rule {
        label: "Cache folder (CACHEDIR.TAG)",
        category: GenericCache,
        risk: Caution,
        note: "Folder marks itself as a cache via CACHEDIR.TAG",
        matcher: Matcher::AnyName { child: M_CACHEDIR },
    },
];

pub fn rule(id: u16) -> &'static Rule {
    &RULES[id as usize]
}

// --------------------------------------------------------------- zones

/// Where name-based rules are allowed to match.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    /// Never match name rules here or in any descendant.
    Out,
    /// Not in the zone, but a descendant might be (e.g. "/", "/Users", "~").
    Maybe,
    /// Inside the project zone.
    In,
}

const BUNDLE_SUFFIXES: &[&str] = &[
    ".app", ".framework", ".bundle", ".plugin", ".appex", ".xpc", ".kext", ".photoslibrary",
    ".xcarchive", ".pkg", ".musiclibrary", ".tvlibrary", ".fcpbundle", ".logicx", ".band",
];

fn is_bundle_name(name: &str) -> bool {
    name == ".Trash" || name == ".Trashes" || BUNDLE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

pub struct Classifier {
    pub home: PathBuf,
    home_rules: HashMap<PathBuf, u16>,
    home_rule_last: HashSet<Vec<u8>>,
    by_name: HashMap<&'static [u8], Vec<u16>>,
    any_name: Vec<u16>,
}

impl Classifier {
    pub fn new(home: PathBuf) -> Self {
        let mut home_rules = HashMap::new();
        let mut home_rule_last = HashSet::new();
        let mut by_name: HashMap<&'static [u8], Vec<u16>> = HashMap::new();
        let mut any_name = Vec::new();
        for (i, r) in RULES.iter().enumerate() {
            let id = i as u16;
            match &r.matcher {
                Matcher::Home(rel) => {
                    let p = home.join(rel);
                    if let Some(last) = p.file_name() {
                        home_rule_last.insert(last.as_encoded_bytes().to_vec());
                    }
                    home_rules.entry(p).or_insert(id);
                }
                Matcher::Name { names, .. } => {
                    for n in names.iter() {
                        by_name.entry(n.as_bytes()).or_default().push(id);
                    }
                }
                Matcher::AnyName { .. } => any_name.push(id),
            }
        }
        Self { home, home_rules, home_rule_last, by_name, any_name }
    }

    /// Zone of a scan root (computed from scratch).
    pub fn zone_for(&self, path: &Path) -> Zone {
        let home = &self.home;
        if home.starts_with(path) {
            return Zone::Maybe;
        }
        if let Ok(rel) = path.strip_prefix(home) {
            return zone_from_rel(rel, &["Library", "Applications"]);
        }
        let volumes = Path::new("/Volumes");
        if path == volumes {
            return Zone::Maybe;
        }
        if let Ok(rel) = path.strip_prefix(volumes) {
            let mut comps = rel.components();
            comps.next(); // volume name
            let rest = comps.as_path();
            if rest.as_os_str().is_empty() {
                return Zone::Maybe;
            }
            return zone_from_rel(rest, &[]);
        }
        Zone::Out
    }

    pub fn child_zone(&self, parent: Zone, child_path: &Path, child_name: &str) -> Zone {
        match parent {
            Zone::Out => Zone::Out,
            Zone::In => {
                if is_bundle_name(child_name) {
                    Zone::Out
                } else {
                    Zone::In
                }
            }
            Zone::Maybe => self.zone_for(child_path),
        }
    }

    /// Classify a directory after it was scanned.
    pub fn classify(
        &self,
        path: &Path,
        name: &[u8],
        parent_markers: u64,
        own_markers: u64,
        zone: Zone,
    ) -> u16 {
        if self.home_rule_last.contains(name) {
            if let Some(id) = self.home_rules.get(path) {
                return *id;
            }
        }
        if zone != Zone::In {
            return NO_RULE;
        }
        if let Some(ids) = self.by_name.get(name) {
            for &id in ids {
                if let Matcher::Name { parent, child, .. } = rule(id).matcher {
                    let parent_ok = parent == 0 || parent_markers & parent != 0;
                    let child_ok = child == 0 || own_markers & child != 0;
                    if parent_ok && child_ok {
                        return id;
                    }
                }
            }
        }
        for &id in &self.any_name {
            if let Matcher::AnyName { child } = rule(id).matcher {
                if own_markers & child != 0 {
                    return id;
                }
            }
        }
        NO_RULE
    }

    /// Re-verify, against the live filesystem, that `path` still matches
    /// rule `id`. Called immediately before moving anything to the Trash.
    pub fn verify(&self, id: u16, path: &Path) -> Result<(), String> {
        let r = RULES.get(id as usize).ok_or("unknown rule")?;
        if r.risk == Risk::InfoOnly {
            return Err("this item is info-only and is never removed by DevDisk".into());
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("path has no valid file name")?;
        match &r.matcher {
            Matcher::Home(rel) => {
                if path != self.home.join(rel) {
                    return Err("path no longer matches the expected cache location".into());
                }
            }
            Matcher::Name { names, parent, child } => {
                if !names.contains(&name) {
                    return Err("folder name does not match the rule".into());
                }
                if !self.path_in_zone(path) {
                    return Err("folder is outside the allowed project area".into());
                }
                let parent_dir = path.parent().ok_or("no parent")?;
                if *parent != 0 && !dir_has_marker(parent_dir, *parent) {
                    return Err("project marker file (e.g. package.json) is missing next to it".into());
                }
                if *child != 0 && !dir_has_marker(path, *child) {
                    return Err("expected build/cache contents are missing inside it".into());
                }
            }
            Matcher::AnyName { child } => {
                if !self.path_in_zone(path) {
                    return Err("folder is outside the allowed project area".into());
                }
                if !dir_has_marker(path, *child) {
                    return Err("identifying marker file is missing inside it".into());
                }
            }
        }
        Ok(())
    }

    /// Both the folder and its parent must be inside the project zone.
    /// Walks from "/" so the logic matches the scanner exactly.
    fn path_in_zone(&self, path: &Path) -> bool {
        let mut parent_zone = Zone::Out;
        let mut zone = self.zone_for(Path::new("/"));
        let mut cur = PathBuf::from("/");
        for comp in path.components().skip(1) {
            let s = match comp.as_os_str().to_str() {
                Some(s) => s,
                None => return false,
            };
            cur.push(s);
            parent_zone = zone;
            zone = self.child_zone(zone, &cur, s);
        }
        parent_zone == Zone::In && zone == Zone::In
    }
}

fn zone_from_rel(rel: &Path, excluded_first: &[&str]) -> Zone {
    let mut first = true;
    for comp in rel.components() {
        let s = match comp.as_os_str().to_str() {
            Some(s) => s,
            None => return Zone::Out,
        };
        if first {
            if s.starts_with('.') || excluded_first.contains(&s) {
                return Zone::Out;
            }
            first = false;
        }
        if is_bundle_name(s) {
            return Zone::Out;
        }
    }
    if first { Zone::Maybe } else { Zone::In }
}
