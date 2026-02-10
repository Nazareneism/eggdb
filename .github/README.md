# .github/ — CI/CD

GitHub Actions workflows for automated testing and publishing.

## Workflows

### `ci.yml` — Continuous Integration

Runs on every push and pull request.

| Job | What It Does |
|-----|--------------|
| **fmt** | `cargo fmt --check` — formatting |
| **clippy** | `cargo clippy -- -D warnings` — linting |
| **test** | `cargo test --workspace` on Ubuntu + macOS |
| **build** | `cargo build --release` on Ubuntu + macOS |
| **fuzz** | 60-second smoke test of both fuzz targets (nightly) |

### `publish-python.yml` — Python Package Publishing

Triggered on GitHub release or manual dispatch.

| Job | What It Does |
|-----|--------------|
| **build** | Builds wheels for 4 platforms (Ubuntu x86_64, Ubuntu aarch64, macOS ARM, macOS Intel) |
| **sdist** | Builds source distribution |
| **publish** | Uploads to PyPI using [Trusted Publisher](https://docs.pypi.org/trusted-publishers/) (OIDC, no API tokens) |

### Publishing a Python Release

1. Create a GitHub release (tag like `v0.1.0`)
2. The workflow builds wheels for all platforms automatically
3. Wheels are published to PyPI as the `eggdb` package
4. Users can then `pip install eggdb`

The PyPI environment (`pypi`) must be configured in the repository settings with
Trusted Publisher credentials.
