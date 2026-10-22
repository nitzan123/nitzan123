# Releasing Prism Studio

Installers are built by GitHub Actions (`.github/workflows/release.yml`)
on Windows, macOS and Linux runners. You don't need any of those machines.

## Make a release

1. Bump `version` in `Cargo.toml` (workspace) and `src-tauri/tauri.conf.json`.
2. Commit, then tag and push:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

3. Wait for the **Release** workflow (about 20–30 minutes). It creates a
   **draft** GitHub release with:

   | Platform | Files |
   |---|---|
   | Windows 10/11 | `Prism Studio_x.y.z_x64_en-US.msi`, `Prism Studio_x.y.z_x64-setup.exe` |
   | macOS 11+ (Apple Silicon and Intel) | `Prism Studio_x.y.z_universal.dmg` |
   | Linux | `.deb` (Debian/Ubuntu), `.rpm` (Fedora/openSUSE), `.AppImage` (any distro) |

4. Check the draft, edit the notes, and press **Publish**.

You can also start the workflow by hand (Actions → Release → Run workflow)
to get a test build without tagging; it is marked as a pre-release draft.

## Code signing

Unsigned installers work, but users get warnings: Windows SmartScreen
("Windows protected your PC") and macOS Gatekeeper ("can't be opened
because Apple cannot check it"). Signing removes those warnings. The
workflow signs automatically as soon as the secrets below exist
(Settings → Secrets and variables → Actions → New repository secret);
each platform is independent.

### macOS (Developer ID + notarization)

Needs an Apple Developer Program membership (USD 99/year).

1. In Xcode or developer.apple.com create a **Developer ID Application**
   certificate and export it with its private key as a `.p12` file.
2. Create an app-specific password at appleid.apple.com.
3. Add these secrets:

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | `base64 -i cert.p12` output |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
   | `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
   | `APPLE_ID` | your Apple ID e-mail (enables notarization) |
   | `APPLE_PASSWORD` | the app-specific password |
   | `APPLE_TEAM_ID` | your 10-character team ID |

The app is built with the hardened runtime, signed, notarized and stapled.

### Windows (Authenticode)

Needs a code-signing certificate from a certificate authority (for example
Sectigo, DigiCert or SSL.com; OV certificates cost roughly USD 200–400 a
year). Since 2023, new OV certificates are issued on hardware tokens or
cloud HSMs; if yours can't be exported as a `.pfx`, use the provider's
cloud signing tool or Azure Trusted Signing instead and replace the
"Windows signing setup" step with its signing command
(`bundle.windows.signCommand` in Tauri's config).

For an exportable certificate, add:

| Secret | Value |
|---|---|
| `WINDOWS_CERTIFICATE` | `base64` of the `.pfx` file (PowerShell: `[Convert]::ToBase64String([IO.File]::ReadAllBytes("cert.pfx"))`) |
| `WINDOWS_CERTIFICATE_PASSWORD` | the `.pfx` password |

The installers and the app are signed with SHA-256 and timestamped.

### Linux

Linux packages are normally not signed by the app author; distributions
and users verify downloads with checksums. GitHub shows a SHA-256 for every
release asset.

## What the installers contain

- The app and its UI. Nothing else is bundled: lens profiles, AI models and
  map tiles are downloaded on request, each with a notice in the app.
- Windows: the WebView2 runtime is installed if missing (online
  bootstrapper). The `.exe` installer installs per user without admin
  rights; the `.msi` is for per-machine installs (e.g. managed PCs).
- File associations for common photo and RAW formats, so Prism Studio
  appears under "Open with".

## Building an installer locally

```bash
npx @tauri-apps/cli@2 build                 # all formats for this OS
npx @tauri-apps/cli@2 build --bundles deb   # just one
```

Output: `target/release/bundle/`.
