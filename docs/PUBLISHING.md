# Publishing NyForge to GitHub from the Omarchy terminal

This is a short, copy-and-paste guide for putting NyForge on GitHub and publishing a release with downloads for
Linux, macOS and Windows. It assumes Omarchy (Arch Linux with Hyprland), but the commands work in any Linux terminal.

Open a terminal with **Super + Enter**. Every command below is typed there.

## 0. Get the project onto your machine

Download `nyforge.bundle` (it holds the whole project and its history) into your home folder, then:

```sh
cd ~
git clone nyforge.bundle nyforge
cd nyforge
```

(If you got `nyforge-source.tar.gz` instead: `tar xzf nyforge-source.tar.gz && cd nyforge && git init -b main`.)

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

## 2. Put your GitHub name in the project

The files contain the placeholder `77nyan` where your GitHub user name (or organization) belongs.
Replace it once (here `yourname` is your GitHub user name):

```sh
cd path/to/nyforge
grep -rn 77nyan --exclude-dir=target --exclude-dir=.git .     # see where it is used
sed -i 's#77nyan#yourname#g' Cargo.toml README.md
```

## 3. Check that it builds

```sh
cargo build --release -p nyforge -p nyforge-gui
cargo test -p nyforge-core -p nyforge
./target/release/nyforge-gui
```

(You need Rust: `sudo pacman -S rustup && rustup default stable`.)

## 4. Create the repository and push

The commits made while building the project carry placeholder author addresses (`dev@nyforge.invalid`). Give them
your identity before the first push, so GitHub shows them as yours:

```sh
git rebase -r --root --exec 'git commit --amend --no-edit --reset-author'
git log --format='%an <%ae>' | sort -u          # should show only you
```


```sh
git status                       # see what changed
git add -A
git commit -m "NyForge 0.1.0: desktop app, installers and release workflow"
gh repo create nyforge --public --source=. --remote=origin --push
```

That creates `github.com/yourname/nyforge`, adds it as `origin` and uploads everything. Use `--private` instead of
`--public` if you want to look at it first (GitHub Actions work on private repositories too, with a monthly limit).

Nothing in the repository is secret: there are no keys or tokens, and `.gitignore` keeps build output (`target/`,
`dist/`) and disk images out. Run `git status` before every commit to see what you are about to publish.

Later changes are just:

```sh
git add -A
git commit -m "What you changed"
git push
```

## 5. Watch the automatic build

Every push runs the **CI** workflow on Linux, macOS and Windows machines at GitHub:

```sh
gh run list --limit 5
gh run watch                     # follow the newest run live
```

Or open the **Actions** tab of the repository. Fix anything red before you release. Two steps
("Flash a RAM disk" on macOS and "Flash a virtual disk" on Windows) are allowed to fail; read their log and
see whether they passed, because that is real information about the macOS and Windows code that has never run
anywhere else.

## 6. Try the release build without publishing

```sh
gh workflow run Release
gh run watch
```

This builds the Linux, macOS and Windows downloads and keeps them as run artifacts (Actions tab, bottom of
the run) without creating a release. Download them and try them on real machines before you tag.

## 7. Publish a release

The version number lives in one place: `version = "0.1.0"` under `[workspace.package]` in `Cargo.toml`.
The tag must match it (`v0.1.0` for `0.1.0`); the workflow refuses to publish otherwise.

```sh
# only when you are releasing a new version, e.g. 0.1.1:
sed -i 's/^version = "0.1.0"/version = "0.1.1"/' Cargo.toml
cargo build                      # refreshes Cargo.lock
git add -A && git commit -m "Version 0.1.1"
git push

git tag v0.1.1
git push origin v0.1.1
```

Pushing the tag starts the **Release** workflow. When it finishes (watch with `gh run watch`), the release appears
with these files, each with a `.sha256` checksum:

* `nyforge-<version>-linux-x86_64.tar.gz` (app and command line)
* `nyforge-<version>-macos-universal.tar.gz` (NyForge.app and command line)
* `nyforge-<version>-windows-x86_64.zip` (app and command line)
* `nyforge-cli-<version>-linux-*.tar.gz` (command line only: older Linux, ARM64, Alpine)

Check it and polish the notes:

```sh
gh release view v0.1.1
gh release edit v0.1.1 --notes "What is new, and what has not been tested yet."
```

Be honest in the notes: say which systems you tested on. The README's "What has been tested" section is the
place to keep that list up to date.

If a release goes wrong, delete it and its tag and try again:

```sh
gh release delete v0.1.1 --cleanup-tag --yes
```

### Releasing by hand (Linux build only)

If you just want the Linux download without waiting for CI:

```sh
tools/package-unix.sh            # writes dist/nyforge-<version>-linux-x86_64.tar.gz (+ .sha256)
gh release create v0.1.1 dist/nyforge-0.1.1-linux-x86_64.tar.gz dist/nyforge-0.1.1-linux-x86_64.tar.gz.sha256 \
  --title "NyForge 0.1.1" --generate-notes
```

macOS and Windows files can only be built on those systems, which is what the workflow does for you.

## 8. Things only you can do

* Code-sign the Windows build and sign and notarize the macOS app. Until then Windows SmartScreen and macOS
  Gatekeeper warn on first start (the README tells users how to continue). Signing needs certificates and an Apple
  developer account, so it is not set up here, and no secret should ever be committed for it: use GitHub
  **Settings > Secrets and variables > Actions** if you add it later.
* Test on real machines with a spare USB stick: macOS and Windows have never run the app before CI.
* Add screenshots of your own to `docs/screenshots` and the README once you have run the app on your systems.
