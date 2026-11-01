# Voxel TUI operator guide

`voxel tui` is the private, persistent control and observability interface for
a Voxel deployment.

## Usage

Build Voxel and prepare images as described in the [project
quickstart](../README.md#quickstart), then start the interface:

```console
$ cargo build -p voxel --bin voxel --features tui
$ pfexec ./target/debug/voxel tui
```

The TUI is the `voxel` crate's opt-in `tui` feature; a build without it has no
`voxel tui` subcommand.

Logs are written to `<resolved workdir>/voxel-tui.log`.

## Views and actions

| Key | Action |
|---|---|
| `Up` / `Down` | Scroll Help or move between confirmation options |
| `PageUp` / `PageDown` | Page Help |
| `Enter` | Select the highlighted confirmation option |
| `?` | Open or close Help |
| `Esc` | Dismiss the topmost confirmation or Help |
| `n` | Reject a confirmation |
| `q` | Quit the TUI |

### Display

Below `48x16`, the TUI displays its minimum-size requirement instead of
clipping an unsafe layout.

## Testing

```sh
cargo test -p voxel --features tui tui::
```
