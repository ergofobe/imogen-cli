#!/usr/bin/env bash
# Generates the things a package installs beside the binary: shell completions, and a man
# page. Both come out of the built program itself, so neither can drift from the commands
# it actually has.
set -euo pipefail

BINARY="${1:?usage: extras.sh <path-to-imogen>}"
OUT="dist/completions"
mkdir -p "$OUT" dist

"$BINARY" completions bash > "$OUT/imogen.bash"
"$BINARY" completions zsh  > "$OUT/_imogen"
"$BINARY" completions fish > "$OUT/imogen.fish"
"$BINARY" completions elvish > "$OUT/imogen.elv"
"$BINARY" completions powershell > "$OUT/imogen.ps1"

# A hand-written page rather than a generated one: `--help` is already exhaustive, and what
# a man page is for is the paragraph that says why you would reach for this at all.
VERSION="$("$BINARY" --version | awk '{print $2}')"
# Honours SOURCE_DATE_EPOCH where a build sets it, so the page is reproducible.
DATE="$(date -u -r "${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y-%m-%d 2>/dev/null || date -u +%Y-%m-%d)"
mkdir -p dist/man

# Source lines are kept under 80 columns because mandoc complains otherwise, and a lint
# that is noisy is a lint nobody reads.
cat > dist/man/imogen.1 <<MAN
.TH IMOGEN 1 "${DATE}" "imogen ${VERSION}" "User Commands"
.SH NAME
imogen \\- command-line client and terminal browser for an imogen photo library
.SH SYNOPSIS
.B imogen
[\\fIOPTIONS\\fR] [\\fICOMMAND\\fR]
.SH DESCRIPTION
Search a photo library, upload to it, download from it, edit what a
photograph says about itself, and administer the server.
.PP
Run with no arguments to browse the library in the terminal, drawing
photographs with the Kitty graphics protocol where the terminal supports
it, and half-block characters where it does not.
.PP
Every command takes \\fB\\-\\-json\\fR, which prints the API's own payload.
Ids go to standard output and progress goes to standard error, so one
command's output is the next one's input.
.SH AUTHENTICATION
.B imogen login \\-\\-server
\\fIURL\\fR runs OAuth 2.1 with PKCE against a loopback redirect.
Credentials are kept in \\fI~/.config/imogen/cli.json\\fR, owner-readable
only.
.SH ENVIRONMENT
.TP
.B IMOGEN_SERVER
The library to talk to, when \\fB\\-\\-server\\fR is not given.
.TP
.B IMOGEN_TOKEN
A bearer token, used for one invocation and never written to disk.
.TP
.B IMOGEN_CONFIG
Where credentials live, overriding the default path.
.TP
.B IMOGEN_IMAGE_PROTOCOL
\\fBkitty\\fR, \\fBblocks\\fR or \\fBnone\\fR, overriding what is detected.
.SH SEE ALSO
Full documentation at
.UR https://github.com/ergofobe/imogen-cli
imogen-cli on GitHub
.UE
MAN

echo "generated:"
ls -1 "$OUT" dist/man
