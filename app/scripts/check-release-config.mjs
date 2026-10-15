// Fails when the installer configuration is inconsistent, so a broken release is caught in CI, not at tag time.
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tauri = join(root, "src-tauri");
const json = (p) => JSON.parse(readFileSync(p, "utf8"));
const problems = [];
const need = (ok, msg) => { if (!ok) problems.push(msg); };

const conf = json(join(tauri, "tauri.conf.json"));
const pkg = json(join(root, "package.json"));
const crate = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(tauri, "Cargo.toml"), "utf8"))?.[1];
need(conf.version === pkg.version && conf.version === crate, `versions differ: tauri.conf.json ${conf.version}, package.json ${pkg.version}, Cargo.toml ${crate}`);
need(/^\d+\.\d+\.\d+$/.test(conf.version), `version ${conf.version} is not major.minor.patch (MSI needs numeric versions)`);

const b = conf.bundle ?? {};
need(b.active === true, "bundle.active must be true");
for (const i of b.icon ?? []) need(existsSync(join(tauri, i)), `icon ${i} is missing`);
need((b.icon ?? []).some((i) => i.endsWith(".ico")) && (b.icon ?? []).some((i) => i.endsWith(".icns")), "an .ico (Windows) and an .icns (macOS) icon are required");
need(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(b.windows?.wix?.upgradeCode ?? ""), "bundle.windows.wix.upgradeCode must be a fixed GUID (it ties upgrades together; never change it)");
need(b.macOS?.entitlements && existsSync(join(tauri, b.macOS.entitlements)), "macOS entitlements file is missing");
const ent = b.macOS?.entitlements ? readFileSync(join(tauri, b.macOS.entitlements), "utf8") : "";
need(ent.includes("disable-library-validation"), "the macOS entitlements must allow loading the Oracle Instant Client (disable-library-validation)");
need(/^[a-z0-9.-]+$/i.test(conf.identifier ?? ""), "identifier is invalid");

const upd = json(join(tauri, "tauri.updater.conf.json"));
need(upd.bundle?.createUpdaterArtifacts === true, "tauri.updater.conf.json must set bundle.createUpdaterArtifacts");
const eps = upd.plugins?.updater?.endpoints ?? [];
need(eps.length > 0 && eps.every((e) => /^https:\/\//.test(e)), "updater endpoints must be https:// addresses");
need(typeof upd.plugins?.updater?.pubkey === "string" && upd.plugins.updater.pubkey.length > 0, "updater pubkey is missing");

// The updater must not be on in ordinary builds: it needs a key, and a placeholder key would break start-up.
need(!conf.plugins?.updater, "tauri.conf.json must not configure the updater; it belongs in tauri.updater.conf.json");
const cargo = readFileSync(join(tauri, "Cargo.toml"), "utf8");
need(/updater\s*=\s*\["dep:tauri-plugin-updater"\]/.test(cargo), "the `updater` cargo feature is missing");

if (problems.length) { for (const p of problems) console.error("✖", p); process.exit(1); }
console.log(`release config ok: v${conf.version}, ${(b.icon ?? []).length} icons, updater feed ${eps[0]}`);
