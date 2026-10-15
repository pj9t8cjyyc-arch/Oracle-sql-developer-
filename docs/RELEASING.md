# Releasing Ora IDE: installers and updates

Everything here is wired up in the repository; two things need **you**: keys/certificates (secrets) and a
real machine to try the installers on. Nothing in this file has been run on Windows or macOS yet: the
build runs in GitHub Actions on `windows-latest` and `macos-latest`.

## What a release produces

| Platform | Artifact | Notes |
|---|---|---|
| Windows | `Ora IDE_<version>_x64_en-US.msi` | WiX MSI, per-machine (needs admin; fine for IT deployment). Silent install: `msiexec /i "Ora IDE_0.1.0_x64_en-US.msi" /qn`. Upgrades replace the old version because `upgradeCode` never changes. |
| macOS | `Ora IDE_<version>_universal.dmg` and `.app.tar.gz` | One build for Apple silicon and Intel. Hardened runtime, notarized when the Apple secrets exist. |
| Both | `latest.json` + `.sig` files | The update feed and its signatures, produced only when `TAURI_SIGNING_PRIVATE_KEY` is set. |

The release is created as a **draft**. Open it on GitHub, try the installers, then publish.

## One-time setup

### 1. Update signing key (needed for in-app updates)

```sh
cd app
npx tauri signer generate -w ~/.tauri/ora-ide.key     # choose a password
```

* Put the **public** key (the `.key.pub` file's content) into `app/src-tauri/tauri.updater.conf.json` → `plugins.updater.pubkey`, replacing `REPLACE_WITH_...`. Commit it.
* Add repository secrets `TAURI_SIGNING_PRIVATE_KEY` (the **private** key file's content) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
* **Back the private key up.** If it is lost, installed copies can never be updated again: they only accept updates signed with this key. If it leaks, anyone holding it can ship code to every installation.

### 2. macOS signing and notarization (so Gatekeeper opens the app)

You need an Apple Developer account and a **Developer ID Application** certificate.

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | the certificate exported as `.p12`, base64-encoded (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | the password you set when exporting |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Company (TEAMID)` |
| `APPLE_ID` | the Apple ID used for notarization |
| `APPLE_PASSWORD` | an **app-specific password** for that Apple ID |
| `APPLE_TEAM_ID` | your 10-character team id |

Without these the DMG is built **unsigned**: macOS will refuse to open it without a right-click → Open.

### 3. Windows signing (optional but recommended)

An unsigned MSI works, but SmartScreen warns and some company policies block it. With a code-signing
certificate (`.pfx`) add `WINDOWS_CERTIFICATE` (base64 of the file) and `WINDOWS_CERTIFICATE_PASSWORD`; the
workflow imports it and signs the MSI with a timestamp. If your company signs with a hardware token, Azure
Trusted Signing or a signing service, sign the downloaded MSI in your own pipeline instead and re-upload it
(the update feed then needs the re-signed file's `.sig`, so regenerate it with `tauri signer sign`).

## Cutting a release

1. Change the version in **three places** to the same number: `app/src-tauri/tauri.conf.json`, `app/src-tauri/Cargo.toml`, `app/package.json`. (CI runs `npm run check:release` and the release job refuses a mismatch.)
2. Commit, then `git tag v0.2.0 && git push origin v0.2.0`.
3. Wait for the `release` workflow, open the draft release, install on a clean Windows and a clean Mac, then **Publish**.
4. Installed apps find the new version through `latest.json` the next time they start (or **v0.1.0** in the status bar → Check now).

## Updates inside a company

* The built-in feed is the GitHub release (`.../releases/latest/download/latest.json`). Many companies block GitHub from desktops.
* To use an internal server: copy `latest.json` and the files it points to onto it, then set the address in **v0.1.0 → Update address** (https only), or set it for everyone by writing the `update.endpoint` setting. The same signed files work anywhere, because the app checks the **signature**, not the server.
* `latest.json` contains absolute download URLs: rewrite them to your internal host before publishing.
* Turn automatic checks off with the checkbox in the same dialog (the setting is `update.auto`).
* Updates are only offered to builds made with `--features updater`; a build from source without the key reports "this build does not include in-app updates".

## Oracle Instant Client (not bundled)

The app loads the Oracle client library at run time (it is not shipped inside the installer, because of
Oracle's redistribution terms and size). Install the 19c or newer Instant Client on each machine; the
connection dialog explains what is missing. On macOS the app is signed with
`disable-library-validation` and `allow-dyld-environment-variables` (see `Entitlements.plist`) so the
hardened runtime can load Oracle's libraries.

## WebView2 on Windows

The MSI downloads the Microsoft WebView2 runtime if it is missing (`downloadBootstrapper`). On machines with
no internet access change `bundle.windows.webviewInstallMode` to `{ "type": "offlineInstaller" }` (adds
about 130 MB) or `{ "type": "skip" }` when IT already deploys WebView2.

## Checks you can run

```sh
cd app && npm run check:release     # versions, icons, GUID, entitlements, update feed
codesign -dv --verbose=4 "Ora IDE.app"                      # macOS: signature
spctl -a -vv "Ora IDE.app"                                  # macOS: Gatekeeper verdict
xcrun stapler validate "Ora IDE_0.1.0_universal.dmg"        # macOS: notarization ticket
signtool verify /pa /v "Ora IDE_0.1.0_x64_en-US.msi"        # Windows: signature
```

## Known limits

* The app identifier is `dev.oraide.ide` (a trailing `.app` conflicts with the macOS bundle extension). It also names the folder where history, worksheets and sandboxes are stored, so **do not change it after the first public release**; a change would look like a fresh install.
* The app icon was produced from a 256 px image; supply a 1024 px square PNG and run `npx tauri icon <file>` for crisp large icons.
* No Linux package is published (the build works; add a `ubuntu-22.04` matrix entry with `--bundles deb,appimage` if wanted).
* Delta (patch) updates do not exist: every update downloads the full installer.
* The updater has not been exercised against a real feed yet.
