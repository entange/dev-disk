# Changelog

## 0.1.0

First public release.

- **Space Lens**: every folder and large file on a drive, sorted by size, with a two-level squarified treemap. Drill in, go back with breadcrumbs or ↑, reveal anything in Finder.
- **Dev Cleanup**: finds regenerable dev/AI folders (node_modules, target, DerivedData, venvs, model caches, …) grouped by ecosystem, each with a Safe / Review / Info-only badge.
- Fast scanning with `getattrlistbulk(2)` on a rayon work-stealing pool; sizes match `du`.
- Cleanup only moves items to the Trash, after a confirmation that lists every path, and re-checks every item right before moving it.
- Universal app (Apple silicon + Intel), macOS 13 or later.
