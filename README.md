# nbs-fractal-web

Provides a web entry point for `nbs-fractal`, letting its capabilities be used without a local command-line environment, and stays in sync with upstream.

This project is AI-generated. Its main behavior is just a reproduction of `nbs-fractal`.

## Local use

Run the executable directly (on a desktop, double-click it). It listens on `127.0.0.1:3021` and asks the `open` crate to launch your default browser. Opening is best-effort: if no browser can be launched (for example on a headless server), the server keeps running silently and the URL is printed to the console.

## Deployment

Download a build for your platform from Releases, or compile it with Cargo, then configure the executable as a system service yourself.

By default it listens on the loopback address `127.0.0.1:3021`; expose it externally through a reverse proxy.
