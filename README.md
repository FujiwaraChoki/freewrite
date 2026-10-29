# hi, this is freewrite

a simple, open-source app to freewrite, for mac, windows and linux.

download latest version [here](https://www.freewrite.io/)

![img](https://i.imgur.com/2ucbtff.gif)

if you wanna make an addition + pr,
or just wanna remix the app for yourself go for it.

- clone repo.
- open in xcode.
- click build.
- done -- you're up and running.

make changes on a pr and i'll run on my end and then build a new version :).

## windows + linux

there's a native windows and linux (arch) version in [`desktop/`](desktop/).

- windows: `cd desktop && cargo build --release`, or grab the installer from the latest ci run.
- arch: `cd desktop/packaging/arch && makepkg -si`.

## cloud sync

sign in from the history sidebar to keep your entries in your account and on every device.
everything is still saved locally first. setup: [`docs/cloud-sync.md`](docs/cloud-sync.md).
