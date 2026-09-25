# Releasing Rustle

How a version gets from a tag to the machines of people who installed Rustle:
cutting the release, the secrets the release workflow uses, what gets signed,
and how installed copies find out about it.

## Cutting a release

1. Bump the version in **both** `package.json` and `[workspace.package]` in
   the root `Cargo.toml`, and let Cargo update `Cargo.lock` (`cargo check`).
   The app reports the `package.json` version (tauri.conf.json points at it);
   the workflow refuses a tag that does not match both.
2. Commit, then tag and push the tag:

   ```sh
   git tag v0.4.0
   git push origin v0.4.0
   ```

   (Actions → Release → Run workflow does the same without a tag push; it
   uses the version in `package.json`.)
3. `.github/workflows/release.yml` builds, in parallel:

   | Runner          | Output                                           |
   | --------------- | ------------------------------------------------ |
   | macOS (arm64)   | `Flow_<v>_aarch64.dmg`, `Flow_aarch64.app.tar.gz` |
   | Ubuntu 24.04    | `.deb`, `.rpm`, `.AppImage`                       |
   | Windows (x64)   | NSIS `-setup.exe`, `.msi`                         |

   into a **draft** release named "Rustle v<version>". With the updater key
   set, each job also uploads `.sig` files and merges its platforms into the
   release's `latest.json`. A last job, "Check latest.json", fails if a
   platform went missing (two jobs finishing at once can race); re-run that
   platform's build job, then the check.
4. Edit the draft's notes and **publish** it. Installed copies only see a
   release once it is published and not a pre-release: the updater asks
   `https://github.com/ceyhuncakir/rustle/releases/latest/download/latest.json`,
   which GitHub serves from the latest published release.

The notes inside `latest.json` (shown in Settings → Updates) are the
workflow's `releaseBody` at build time, not what you type into the draft
later.

## How installed copies update

Rustle's Rust side (`src-tauri/src/updates.rs`) uses `tauri-plugin-updater`.
Every download is checked against the public key in
`src-tauri/tauri.conf.json` (`plugins.updater.pubkey`) before anything is
installed, and `requireSignedVersion` makes the signature name the version,
so a tampered `latest.json` cannot pass off an older signed build as a new
one.

| How Rustle was installed              | Settings → Updates                                   |
| ----------------------------------- | ---------------------------------------------------- |
| AppImage (started normally, `$APPIMAGE` set) | **Install and restart**: replaces the AppImage file in place and restarts. The AppImage's folder must be writable; if it is not, Rustle says so and links the releases page. |
| `.deb` / `.rpm`                     | Shows the new version; points at the releases page (the package manager owns those files). |
| Windows installer (NSIS or MSI)     | **Install and restart**: runs the new installer in passive mode, which starts Rustle again. |
| macOS app                           | **Install and restart**: replaces `Rustle.app` (asks for an administrator password if its folder is not writable) and restarts. |
| Built from source (`scripts/install-app.sh`, `cargo run --release`) or a debug build | "Check for updates" reports what is out; nothing is offered and nothing is checked automatically. |

The install kind comes from the bundle type Tauri's bundler stamps into the
binary; `--no-bundle` builds (what `scripts/install-app.sh` makes) carry
none, which is how they are told apart.

**Automatic checks.** A copy installed from a release checks at most once a
day (the last check is kept in `<data dir>/update-check.json`), 90 seconds
after start and then hourly to see whether a day has passed. When it finds a
version it has not mentioned before, it shows one notification; nothing is
downloaded until someone presses Install. The request is a plain GET of
`latest.json` from GitHub. Settings → Updates → "Check automatically", or
`check_updates = false` under `[desktop]` in the config file, turns it off.

## Secrets

Set them under Settings → Secrets and variables → Actions, or with `gh`.
Every one is optional: without it that part is skipped with a warning in
the run, and the release still builds.

| Secret | Used for | Without it |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Signing update artifacts; switches on `latest.json` | No `latest.json`: installed copies are never offered the release |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The key's password | Leave unset: the current key has an empty password |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | Developer ID signing on macOS | App unsigned; Gatekeeper blocks it |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Notarisation | Signed but not notarised; Gatekeeper still warns |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Authenticode signing on Windows | Installer unsigned; SmartScreen warns |

### Updater key

The key pair was generated with

```sh
pnpm tauri signer generate --ci -p "" -w ~/.tauri/rustle-updater.key
```

- private key: `~/.tauri/rustle-updater.key` (mode 0600, empty password)
- public key: `~/.tauri/rustle-updater.key.pub`, copied into
  `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`

Upload the private key:

```sh
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/rustle-updater.key
```

**Back it up now**, somewhere offline (a password manager is fine). Every
installed copy trusts exactly this key. If it is lost, no future release can
be signed so that existing installs accept it: everyone would have to
download the next version by hand. If it leaks, whoever has it and can also
put a `latest.json` on this repository's releases can ship code to every
install; rotate it.

To rotate (or to add a password), generate a new pair, put the **new**
public key into `tauri.conf.json`, and ship that release signed with the
**old** private key; only after it is out, replace the secret with the new
private key. Copies that skip that release can no longer update themselves.

To sign a local build the way CI does:

```sh
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/rustle-updater.key)" TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  pnpm tauri build --features webgpu --config src-tauri/tauri.webgpu.linux.conf.json \
  --config src-tauri/tauri.release.conf.json
```

Plain `pnpm tauri build` and `scripts/install-app.sh` need no key:
`bundle.createUpdaterArtifacts` is only in `src-tauri/tauri.release.conf.json`,
which only the release workflow passes (and only when the key secret exists).

### Apple (macOS signing and notarisation)

Needs a paid Apple Developer Program membership.

1. Create a **Developer ID Application** certificate: Xcode → Settings →
   Accounts → Manage Certificates → +, or developer.apple.com →
   Certificates with a signing request from Keychain Access.
2. In Keychain Access, export the certificate *with its private key* as a
   `.p12`, with a password.
3. Upload it and its identity:

   ```sh
   base64 -i DeveloperID.p12 | tr -d '\n' | gh secret set APPLE_CERTIFICATE
   gh secret set APPLE_CERTIFICATE_PASSWORD        # the .p12 password
   security find-identity -v -p codesigning        # copy the full name
   gh secret set APPLE_SIGNING_IDENTITY --body "Developer ID Application: Your Name (TEAMID1234)"
   ```

4. For notarisation, create an app-specific password at account.apple.com →
   Sign-In and Security → App-Specific Passwords, then:

   ```sh
   gh secret set APPLE_ID --body "you@example.com"
   gh secret set APPLE_PASSWORD                    # the app-specific password
   gh secret set APPLE_TEAM_ID --body "TEAMID1234"
   ```

   (Tauri also accepts an App Store Connect API key, `APPLE_API_KEY`,
   `APPLE_API_ISSUER` and `APPLE_API_KEY_PATH`; the workflow does not pass
   those.)

The three signing values are passed to the build only when all three are
set: the bundler tries to import `APPLE_CERTIFICATE` whenever the variable
exists, even empty.

What gets signed: the bundler signs inside out with the hardened runtime,
the Dawn library first (`Contents/Frameworks/libwebgpu_dawn.dylib`, from
`tauri.webgpu.macos.conf.json`; ONNX Runtime is linked statically), then
`Rustle.app` with `src-tauri/Entitlements.plist`, then the DMG; it notarises
and staples the app. The updater's `Flow_aarch64.app.tar.gz` is made from
the signed app. Entitlements: `device.audio-input` (the microphone under the
hardened runtime; `device.microphone` is its App Sandbox twin, harmless
here) and `cs.allow-jit`. Rustle sends no Apple Events, so there is no
`NSAppleEventsUsageDescription`; `Info.plist` carries
`NSMicrophoneUsageDescription`.

Unsigned macOS builds (no secrets) open only after
`xattr -dr com.apple.quarantine /Applications/Rustle.app`; Gatekeeper
otherwise reports them as damaged.

### Windows (Authenticode)

The workflow takes a code signing certificate as a base64 `.pfx`:

```sh
base64 -w0 rustle-codesign.pfx | gh secret set WINDOWS_CERTIFICATE
gh secret set WINDOWS_CERTIFICATE_PASSWORD
```

(On Windows: `[Convert]::ToBase64String([IO.File]::ReadAllBytes("rustle-codesign.pfx"))`.)
It imports the certificate into the runner's `CurrentUser\My` store and
passes the bundler one more `--config` with `bundle.windows`:
`certificateThumbprint` (from the import), `digestAlgorithm: sha256`,
`timestampUrl: http://timestamp.digicert.com` and `tsp: true` (RFC 3161).

What gets signed: `rustle.exe`, the DLLs bundled from
`tauri.webgpu.windows.conf.json` that are not signed already (Dawn's
`webgpu_dawn.dll`; Microsoft's `dxcompiler.dll` and `dxil.dll` usually are),
the NSIS installer with its uninstaller and plugins, and the MSI.

Since June 2023, certificate authorities issue OV and EV code signing keys
only on hardware tokens or cloud HSMs, so a new certificate usually cannot
be exported as a `.pfx`. This path suits a certificate you already hold as a
`.pfx`. For a new one, use a cloud signing service through
`bundle.windows.signCommand` instead, such as:

**Azure Trusted Signing** (not wired up; check Microsoft's eligibility rules
for individuals and organisations first):

1. In Azure, create a Trusted Signing account, complete identity
   validation and create a certificate profile.
2. Create an app registration (service principal) and give it the
   "Trusted Signing Certificate Profile Signer" role on the account.
3. Store `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` and `AZURE_TENANT_ID` as
   secrets and pass them to the tauri-action step's `env`.
4. On the Windows runner, `cargo install trusted-signing-cli`, and add to
   the generated config (instead of `certificateThumbprint`):

   ```json
   { "bundle": { "windows": { "signCommand":
     "trusted-signing-cli -e https://<region>.codesigning.azure.net -a <account> -c <profile> -d Rustle %1" } } }
   ```

   The bundler calls it once per file, `%1` being the file.

SmartScreen reputation comes with downloads over time either way.

### Linux

Nothing is signed for Linux beyond the updater signature: the `.deb` and
`.rpm` are unsigned packages (no repository), and the AppImage carries no
embedded GPG signature. The AppImage's `.sig` is what the updater checks.

## Signing at a glance

| Artifact | Signed with | Secrets |
| --- | --- | --- |
| macOS `.app` (+ Dawn dylib), `.dmg` | Developer ID, hardened runtime, notarised | `APPLE_*` |
| Windows `rustle.exe`, DLLs, `-setup.exe`, `.msi` | Authenticode (SHA-256, RFC 3161 timestamp) | `WINDOWS_CERTIFICATE*` |
| Linux `.deb`, `.rpm`, `.AppImage` | (none) | |
| Update artifacts (`.app.tar.gz`, `.AppImage`, `-setup.exe`, `.msi`, `.deb`, `.rpm`) and their `.sig` | minisign (updater key) | `TAURI_SIGNING_PRIVATE_KEY` |

## Testing an update

With a published release vN installed:

1. Publish vN+1 with the updater key set.
2. AppImage, Windows, macOS: Settings → Updates → Check for updates →
   Install and restart. Rustle comes back on vN+1 with its settings intact.
3. `.deb`/`.rpm`: the check shows vN+1 and "Open releases page".
4. A copy built with `scripts/install-app.sh`: the check reports vN+1 and
   says to rebuild; no notification ever appears.
5. With "Check automatically" on, delete `<data dir>/update-check.json` and
   restart Rustle: about 90 seconds later one notification names vN+1, and
   restarting again does not repeat it.
