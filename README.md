# daily

`daily` is a window manager for X11 desktop, mainly for my daily use.

## Usage

1. `cargo install --path .`
2. add the following line to the end of your `.xinitrc`:
```
exec /home/you/.cargo/bin/daily2
```
3. `startx`

To leave the log messages, change the `.xinitrc` as follows:
```
RUST_LOG=daily2 exec /home/you/.cargo/bin/daily2 >/tmp/daily2.log 2>&1
```
