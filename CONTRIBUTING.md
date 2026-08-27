# Contributing

```bash
git clone https://github.com/ergofobe/imogen-sdk    # a path dependency, beside this one
git clone https://github.com/ergofobe/imogen-cli
cd imogen-cli

cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

The client library lives in [imogen-sdk](https://github.com/ergofobe/imogen-sdk) and is
referenced by path, so the two repositories are checked out side by side. Anything that
touches the wire — a new endpoint, a new field — belongs there rather than here: this
program should contain no knowledge of HTTP at all.

## What to keep in mind

**Two audiences.** Every command answers twice. A person gets a table; `--json` gets the
API's own payload, unrenamed. Ids go to stdout and everything else to stderr, so one
command's output is the next one's input. A change that only looks right in a terminal is
half a change.

**Nothing irreversible without being asked.** A command that names ids acts on them —
naming them is the confirmation. A command that selects by filter asks first, and refuses
rather than prompting when there is no terminal to answer: a script gets an error telling
it to pass `--yes`, never a question nobody will read.

**Pictures are composed, not mixed.** The terminal browser draws its layout with cells and
leaves holes; photographs are written into those holes afterwards, by the Kitty graphics
protocol or by half-block characters. Neither half knows about the other.

Run `cargo test` before opening a pull request. `cli.rs` has a test that asserts clap's own
consistency check over every subcommand — it catches a flag declared twice, which is a
panic in a debug build and silently the wrong argument in a release one.
