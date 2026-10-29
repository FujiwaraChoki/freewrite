# freewrite for Windows and Linux

A native port of the macOS app, written in Rust with [egui](https://github.com/emilk/egui).
It compiles to a single executable: no browser engine and no runtime to install.

Same app, same rules: a page, a 15 minute timer (scroll on it to change it),
no backspace mode, fonts, dark mode, history, PDF export and "Chat".
Entries are the same `[uuid]-[date].md` files in `Documents/Freewrite`,
so a folder can move between macOS, Windows and Linux.

## Build

You need a recent stable Rust (1.95 or newer, via [rustup](https://rustup.rs)).

```bash
cd desktop
cargo run --release
```

### Windows

```powershell
cd desktop
cargo build --release
# target\release\freewrite.exe
```

For an installer, install [Inno Setup](https://jrsoftware.org/isinfo.php) and run
`iscc packaging\windows\freewrite.iss`; it writes `target\installer\freewrite-setup-<version>.exe`.

### Arch Linux

```bash
cd desktop/packaging/arch
makepkg -si
```

This builds the package from your checkout and installs `freewrite` with its
desktop entry and icon. It runs on Wayland and X11.

## Cloud sync

Signing in (History sidebar → "Sign in to sync") keeps your entries in your
account so they follow you between devices. Writing is still saved locally
first and the app works offline. See [`docs/cloud-sync.md`](../docs/cloud-sync.md)
to set up the backend.

The app finds the server in this order:

1. `FREEWRITE_SUPABASE_URL` and `FREEWRITE_SUPABASE_ANON_KEY` environment variables
2. `cloud.json` in the config folder (`%APPDATA%\freewrite` or `~/.config/freewrite`):
   `{ "url": "https://xyz.supabase.co", "anon_key": "sb_publishable_..." }`
3. The same two variables set at build time, which bakes them into the binary.

Your sign-in is kept in Windows Credential Manager or the Secret Service on
Linux, falling back to a private `session.json` when neither is available.

## Tests

```bash
cargo test
```

The sync engine is tested against an in-memory server simulating several
devices. To run the end-to-end test against a real project (with email
confirmation turned off):

```bash
FREEWRITE_E2E_URL=https://xyz.supabase.co FREEWRITE_E2E_KEY=sb_publishable_... cargo test -- --ignored
```
