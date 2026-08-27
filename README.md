<div align="center">
  <h1>imogen-cli</h1>
  <p><strong>Your photo library, from the command line.</strong></p>
</div>

A client for an [imogen](https://github.com/ergofobe/imogen-server) photo library. Search
it, upload to it, download from it, edit what a photograph says about itself, and
administer the server — from a shell, from a script, or from an agent.

Run it with no arguments and it becomes a terminal browser that draws your photographs
using the Kitty graphics protocol.

```bash
cargo install --path .          # or: cargo build --release
imogen login --server https://photos.example.com
imogen
```

---

## Two audiences

Every command answers twice.

```console
$ imogen search harbour
ID                                    TAKEN       FILENAME         KIND   SIZE
2fcdb6cf-fc39-4992-99ee-ca56db4e05a7  2019-07-14  cliff-path.jpg   photo  2.4 MiB  ★
```

```console
$ imogen search harbour --json | jq -r '.items[].id'
2fcdb6cf-fc39-4992-99ee-ca56db4e05a7
```

`--json` prints the API's own payload — unrenamed, uncollapsed — so a program never has to
parse a table that was designed to be read rather than to be stable. Ids go to stdout and
progress goes to stderr, which means one command's output is the next one's input:

```bash
imogen ls --favorite --ids | xargs imogen edit --archive
imogen ls --query "cornwall" --ids | xargs imogen album add "Best of 2019"
```

**Nothing irreversible happens without being asked.** A command given ids acts on them —
naming them is the confirmation. A command that selects by filter asks first, and where
there is no terminal to answer it refuses outright rather than hanging on a prompt nobody
will read:

```console
$ imogen trash --query cliff < /dev/null
error: Move 1 photograph to the trash? Pass --yes to go ahead.
```

---

## Signing in

Authorization code with PKCE against a loopback redirect — the flow RFC 8252 prescribes
for a command-line tool. Your browser handles the login; the code comes back to a server
that exists for the few seconds the flow takes. Nothing is pasted by hand.

```bash
imogen login --server https://photos.example.com
```

On a machine with no browser, `--no-browser` prints the URL to open somewhere else. If you
already hold a token, `--with-token` skips the flow entirely, and `IMOGEN_TOKEN` works for
a single command without being written to disk at all.

Several libraries coexist as named profiles:

```bash
imogen login --server https://photos.example.com --name home
imogen login --server https://family.example.net --name family
imogen --profile family ls
imogen profiles --set-default home
```

Credentials live in `~/.config/imogen/cli.json`, written owner-only. The access token is
refreshed in place when it goes stale, so a long-running script never has to think about
expiry. That file is separate from `imogen-mcp`'s: revoking one does not sign the other
out.

---

## Uploading

Flags describe a whole run, which is what you want when typing:

```bash
imogen upload ~/Pictures/cornwall --recursive --album "Cornwall 2019" --favorite
```

A **manifest** describes each file separately, which is what a script moving a library in
from somewhere else wants — it has already worked out each photograph's date, place and
description, and needs somewhere to put them. One JSON object per line, using the API's own
field names:

```jsonl
{"path":"IMG_1234.JPG","capturedAt":1717233000,"favorite":true,"album":"Cornwall 2019",
 "location":{"latitude":50.1109,"longitude":-5.5372},"description":"Fishing boats at dawn"}
{"path":"IMG_1235.JPG","capturedAt":"2019-07-15T08:30:00Z","filename":"the-real-name.jpg"}
```

```bash
your-exporter | imogen upload --manifest - --report done.jsonl
```

- `capturedAt` may be ISO-8601, a plain date, or seconds since the epoch.
- `filename` is what the photograph is called in the library, whatever the file on disk is
  named — for restoring a name an export truncated.
- `deviceAssetId` is a stable id of your own, so the same photograph sent again from
  another machine is recognised rather than stored twice.
- `--report` appends one line per file **as it settles**, so an interrupted run leaves a
  usable record and your wrapper decides for itself what to retry.

Uploads are idempotent by content: re-running a manifest costs a checksum, not a duplicate.
Files at or above 64 MB switch to a resumable session automatically, so a dropped
connection costs one chunk rather than the whole video.

A misspelled field is refused rather than ignored — silently dropping `capturedat` would
import a whole library with the wrong dates.

## Downloading

```bash
imogen download --album "Cornwall 2019" --out ./export
imogen download --query harbour --variant preview --layout "{id}{ext}" --out ./thumbs
```

`{yyyy} {mm} {dd} {id} {name} {stem} {ext} {album}` are filled in. Files are streamed to
disk through a `.part`, so an interrupted download never leaves something that looks
finished; a name that is already there is skipped unless you pass `--overwrite`, and two
photographs that honestly share a filename get a suffix rather than one overwriting the
other.

## Editing

```bash
imogen edit <id> --description "Fishing boats at dawn" --location 50.1109,-5.5372
imogen edit <id> --captured-at 1987-08-14 --favorite
imogen edit --query "scan" --captured-at 1987-08-14 --yes
```

`--favorite` on `edit` means *make it a favourite*; selecting favourites is `imogen ls
--favorite --ids` piped in. The two never mean the same word.

## Everything else

```
imogen ls · search · show · stats · timeline    the library
imogen upload · download · edit · trash · restore
imogen album    list · show · create · update · delete · add · remove
imogen share    create · show · revoke            public links
imogen people   list · show · name · merge · hide · faces · status
imogen account  show · update · password · logout-everywhere
imogen admin    users · invites · queue · clients · sessions · storage · settings · shares
```

`imogen <command> --help` describes each. `imogen completions <shell>` prints a completion
script.

---

## The terminal browser

With no arguments, imogen draws your library.

```
┌ imogen · library ────────────────────────── 12,431 photographs ─┐
│  ┌────────┐  ┌────────┐  ┌────────┐  ┌────────┐                 │
│  │ photo  │  │ photo  │  │ photo  │  │ photo  │                 │
│  │2019-07 │  │2019-07 │  │2019-07★│  │2019-07 │                 │
│  └────────┘  └────────┘  └────────┘  └────────┘                 │
└ harbour.jpg · 2019-07-14 · 2.4 MiB ★        ? help  q quit ─────┘
```

| | |
|---|---|
| `↑ ↓ ← →` `h j k l` | move |
| `enter` · `escape` | look at it · back |
| `/` | search |
| `f` · `e` · `d` · `r` | favourite · archive · trash · restore |
| `i` · `a` | details · albums |
| `u` | pick files to upload |
| `1` `2` `3` `4` | library · favourites · archive · trash |
| `g` `G` · `R` · `?` | first · last · reload · keys |

### Picking files

`u` opens a file picker rather than asking you to remember a path.

```
┌ ~/Pictures/cornwall ─────────────────┐┌──────────────────────┐
│  nested/                             ││                      │
│  notes.txt                     1.2 K ││   [ the photograph   │
│✓ harbour.jpg                   2.4 M ││     under the        │
│  cliff-path.jpg                3.1 M ││     cursor ]         │
└──────────────────────────────────────┘└──────────────────────┘
 1 item  ·  space pick · enter open · u upload · esc back
```

The pane on the right draws whatever the cursor is on, so you can tell one `IMG_4471.JPG`
from another without leaving the terminal. A file it cannot decode — a HEIC, a RAW — says
so and notes that it will still upload; only the preview is missing, not the capability.

`space` picks and unpicks, `enter` opens a folder or picks a file, `h` steps back out, `a`
picks every photograph in the folder and `A` clears the lot. `.` shows hidden files, `~`
goes home, `/` takes a typed path when you do know it, and `u` sends what is picked — or
just what the cursor is on, if you picked nothing. `?` lists all of it. Nothing uploads
until you press `u`, so `enter` one time too many costs you nothing.

Folders are walked; a single file is taken at its word, so a photograph with an extension
imogen does not usually look for can still be sent by naming it. Files go four at a time,
the footer counts them off, and the grid reloads when the run ends so new photographs
appear where their dates put them rather than at the end. Upload while browsing an album
and they are filed into it.

A photograph is stored before it is processed, so for a second or two after it lands there
is no thumbnail to show — the server is still making one. The browser keeps asking about
anything it can see that is not finished yet, and fills the picture in when it appears.
Nothing needs reloading.

For anything with metadata to carry — a date, a place, a caption — use `imogen upload` from
the shell instead: the browser sends the files and nothing else.

Photographs are drawn with the **Kitty graphics protocol** where the terminal has it —
kitty, Ghostty, WezTerm, Konsole — and with half-block characters and 24-bit colour
everywhere else. The layout is drawn as cells with holes left in it, and the pictures are
written into those holes afterwards; neither half knows about the other.

Override the guess with `IMOGEN_IMAGE_PROTOCOL=kitty|blocks|none`, or set `IMOGEN_NO_IMAGES`
to draw nothing at all.

---

## For agents

An agent driving this program should know four things:

1. **`--json` on everything.** The payload is the API's own. Errors come back as
   `{"error": "...", "causes": [...]}` on stdout in JSON mode, and the exit status is
   non-zero.
2. **stdout is data, stderr is commentary.** `--quiet` removes the commentary entirely.
3. **`--yes` is required** for anything destructive selected by filter, because there is no
   terminal to answer a prompt. Naming ids explicitly never prompts.
4. **`IMOGEN_TOKEN` and `IMOGEN_SERVER`** authenticate a single invocation without touching
   the credentials file.

```bash
imogen --json stats
imogen --json ls --query "birthday" --after 2019-01 --before 2019-12 --all
imogen --json upload --manifest - --report /tmp/done.jsonl < manifest.jsonl
```

---

## Environment

| Variable | What it does |
|---|---|
| `IMOGEN_SERVER` | The library to talk to, when `--server` is not given |
| `IMOGEN_PROFILE` | Which saved login to use |
| `IMOGEN_TOKEN` | A bearer token, used for one invocation and never written down |
| `IMOGEN_CONFIG` | Where credentials live, overriding `~/.config/imogen/cli.json` |
| `IMOGEN_IMAGE_PROTOCOL` | `kitty`, `blocks` or `none`, overriding what is detected |
| `IMOGEN_NO_IMAGES` | Draw no pictures at all |
| `NO_COLOR` | Colour off, as everywhere else |

## Building

```bash
cargo build --release
```

The client library is [imogen-sdk](https://github.com/ergofobe/imogen-sdk), referenced by
path, so check it out beside this repository. Everything that touches the wire lives there;
this program contains no HTTP at all.

## Licence

AGPL-3.0-or-later.
