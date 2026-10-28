# Publishing NyForge 0.1.7 to GitHub (fresh start)

This guide starts from nothing: a new, empty GitHub repository and the `nyforge-0.1.7-source.tar.gz` file. It is
copy-and-paste, and it publishes a release with downloads for Linux and Windows.
It assumes Omarchy (Arch Linux with Hyprland), but the commands work in any Linux terminal.

Open a terminal with **Super + Enter**. Every command below is typed there. The GitHub user name used here is
`77nyan` and the repository name is `nyforge`.

## 1. One-time setup

Check that Git and the GitHub command-line tool are installed:

```sh
git --version
gh --version
```

If either is missing:

```sh
sudo pacman -S git github-cli
```

Tell Git who you are (this goes on every commit, and is public once pushed):

```sh
git config --global user.name  "Your Name"
git config --global user.email "you@example.com"
```

Log in to GitHub. Pick **GitHub.com**, **HTTPS** (or SSH if you already use a key), and **Login with a web browser**:

```sh
gh auth login
gh auth status          # should say you are logged in
```

`gh` stores the login itself. Never paste a token or password into a file in the project.

You also need Rust to test the build (`sudo pacman -S rustup && rustup default stable`).

## 2. Unpack the project

Download `nyforge-0.1.7-source.tar.gz` into your Downloads folder, then:

```sh
cd ~
tar xzf ~/Downloads/nyforge-0.1.7-source.tar.gz
mv nyforge-0.1.7 nyforge
cd nyforge
ls                                   # Cargo.toml, README.md, install.sh, crates, ...
```

The folder has no Git history. It is a clean start, and the repository you create below begins with one commit.
The version is already set to 0.1.7 and the repository address already points to `github.com/77nyan/nyforge`.

## 3. Check that it builds

```sh
cargo build --release -p nyforge -p nyforge-gui
cargo test -p nyforge-core -p nyforge
./target/release/nyforge-gui         # click around; the cog at the top right opens Settings
```

The first build takes a few minutes. You can also try the installer from the folder (it must be run with sudo, and
it builds the app first):

```sh
sudo ./install.sh
nyforge --version                    # nyforge 0.1.7
sudo nyforge-uninstall               # take it off again if you only wanted to try it
```

## 4. Create the repository and push

```sh
git init -b main
git add -A
git status                           # look at what is about to be published
git commit -m "NyForge 0.1.7"
gh repo create nyforge --public --source=. --remote=origin --push
```

That creates `github.com/77nyan/nyforge`, adds it as `origin` and uploads everything. Use `--private` instead of
`--public` if you want to look at it first (GitHub Actions work on private repositories too, with a monthly limit).

If `gh repo create` says the name is taken, you already have a repository called `nyforge`. Either delete it on
GitHub first (**Settings > Danger Zone**) or pick another name and change the address in `Cargo.toml`.

Nothing in the repository is secret: there are no keys or tokens, and `.gitignore` keeps build output (`target/`,
`dist/`) and disk images out. Run `git status` before every commit to see what you are about to publish.

Later changes are just:

```sh
git add -A
git commit -m "What you changed"
git push
```

## 5. Watch the automatic build

Every push runs the **CI** workflow on Linux and Windows machines at GitHub:

```sh
gh run list --limit 5
gh run watch                         # follow the newest run live
```

Or open the **Actions** tab of the repository. Fix anything red before you release. The step
"Flash a virtual disk" on Windows is allowed to fail; read its log and see whether it passed, because that is
real information about the Windows code.

## 6. Try the release build without publishing

```sh
gh workflow run Release
gh run watch
```

This builds the Linux and Windows downloads and keeps them as run artifacts (Actions tab, bottom of
the run) without creating a release. Download them and try them on real machines before you tag.

## 7. Publish the release

The version number lives in one place: `version = "0.1.7"` under `[workspace.package]` in `Cargo.toml`.
The tag must match it (`v0.1.7` for `0.1.7`); the workflow refuses to publish otherwise.

```sh
git tag v0.1.7
git push origin v0.1.7
gh run watch
```

Pushing the tag starts the **Release** workflow. When it finishes, the release appears with these files, each with a
`.sha256` checksum:

* `nyforge-0.1.7-linux-x86_64.tar.gz` (app and command line)
* `nyforge-0.1.7-windows-x86_64.zip` (app and command line)
* `nyforge-cli-0.1.7-linux-*.tar.gz` (command line only: older Linux, ARM64, Alpine)

Check it and write the release notes:

```sh
gh release view v0.1.7
gh release edit v0.1.7 --notes "Huge update! A new optional tool, NyTest (speed, health and capacity tests for USB sticks and SD cards; switch it on in Settings, the cog at the top right), a Settings page to reorder the start screen, a new NyBoot menu with custom themes and Memtest86+, a refreshed app, and many fixes. Full list: CHANGELOG.md"
```

Be honest in the notes: say which systems you tested on. The README's "What has been tested" section is the
place to keep that list up to date.

If a release goes wrong, delete it and its tag and try again:

```sh
gh release delete v0.1.7 --cleanup-tag --yes
```

### Releasing by hand (Linux build only)

If you just want the Linux download without waiting for CI:

```sh
tools/package-unix.sh                # writes dist/nyforge-0.1.7-linux-x86_64.tar.gz (+ .sha256)
gh release create v0.1.7 dist/nyforge-0.1.7-linux-x86_64.tar.gz dist/nyforge-0.1.7-linux-x86_64.tar.gz.sha256 \
  --title "NyForge 0.1.7" --notes "Huge update! See CHANGELOG.md for the full list."
```

The Windows file can only be built on Windows, which is what the workflow does for you.

## 8. Next versions

Change the version in `Cargo.toml` (for example to 0.1.8), build once so `Cargo.lock` follows, commit, push, and tag:

```sh
sed -i 's/^version = "0.1.7"/version = "0.1.8"/' Cargo.toml
cargo build
git add -A && git commit -m "Version 0.1.8" && git push
git tag v0.1.8 && git push origin v0.1.8
```

## 9. Things only you can do

* Code-sign the Windows build. Until then Windows SmartScreen warns on first start (the README tells users how to
  continue). Signing needs a certificate, so it is not set up here, and no secret should ever be committed for it: use GitHub
  **Settings > Secrets and variables > Actions** if you add it later.
* Test on real machines with a spare USB stick: Windows has never run the app before CI. The Windows
  "finished" notification in particular has only been checked by reading the code.
