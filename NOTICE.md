# NOTICE

ghostty-rs carries code and assets from the projects below.

## Ghostty

ghostty-rs is a file-for-file Rust port of [Ghostty](https://github.com/ghostty-org/ghostty) at revision `93967b7f9478e94dcbb42125c8a90f34feb40d75`. The Rust sources mirror Ghostty's Zig sources, and the macOS app, the C headers, the examples and the runtime resources come from Ghostty.

Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors. MIT License; the full text is in [LICENSE](LICENSE).

## rustty

The port started from [rustty](https://github.com/vestergaardtechnology/rustty), first published as ghrossty, at revision `22c6649679632701e7f6386c8825a1bc21a0a578`.

## Bundled code and fonts

- Dear ImGui, `vendor/dcimgui/imgui`: MIT, Copyright (c) 2014-2025 Omar Cornut. Full text in `vendor/dcimgui/imgui/LICENSE.txt`.
- `vendor/dcimgui/bindings`: generated from Dear ImGui by [Dear Bindings](https://github.com/dearimgui/dear_bindings).
- glad, `vendor/glad`: generated OpenGL and EGL loaders, `(WTFPL OR CC0-1.0) AND Apache-2.0`. The Khronos headers are Copyright The Khronos Group Inc., Apache-2.0.
- stb_image and stb_image_resize, `src/stb`: Sean Barrett, public domain or MIT.
- Nerd Fonts symbols, `src/font/res/nerd_fonts_symbols_only`: MIT, Copyright (c) 2014 Ryan L McIntyre.
- Test fonts in `src/font/res`: OFL-1.1, MIT and BSD-2-Clause. Each font's copyright and license is listed in `src/font/res/README.md`.
