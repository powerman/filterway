# Filterway

Wayland socket proxy that can do minor changes to messages for any programs
that use its downstream socket.

This allows you to do things like create a proxy Wayland socket to mount in a container
and write compositor decoration rules that are specific to the container windows.

Current filters:

- Replace or prefix `app_id` -
  this can help writing compositor rules targeting programs running on a filterway instance.
- Replace or prefix `title` - this may be helpful if nesting compositors,
  since compositors don't expect their title to be used and don't set useful titles.

# How to use it

Your main compositor will have created something like `/run/user/1000/wayland-0` where `1000` is
your user ID.

1. Build `filterway` with `cargo build`.

   Note, socket ancillary data (required by Wayland protocol) requires unstable rust currently.
   If you use `rustup` to manage rust it should read the `rust-toolchain.toml` file and compile
   accordingly.

2. Run `filterway --upstream /run/user/1000/wayland-0 --downstream /run/user/1000/wayland-filtered --app-id org.example.testid`

   Run `filterway --help` for details.

3. Run Wayland applications or another compositor with `WAYLAND_DISPLAY=wayland-filtered`

# Acknowledgements

This project is a fork of [andrewbaxter/filterway](https://github.com/andrewbaxter/filterway/), licensed under ISC.

I'm grateful to Andrew Baxter for the original implementation and inspiration.
The original project appears to be inactive,
and since I needed to introduce substantial changes,
a separate project fork proved to be a better fit than a pull request workflow.
The original author's code remains under the [ISC license](andrewbaxter-license.txt).
