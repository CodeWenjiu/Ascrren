# MSU2 Slint spike

Headless Slint experiment for the 160×80 MSU2 panel.

It verifies that Slint's software renderer can drive a custom display backend without
a windowing system:

- renders a 160×80 UI into an RGB565 framebuffer;
- uses `RepaintBufferType::ReusedBuffer` so Slint redraws only dirty areas;
- prints Slint's dirty bounding box and an app-level 16×16 tile diff;
- writes the final frame to `target/msu2_dump/slint_frame.bmp`.

Run:

```bash
nix develop --command cargo run -q -p msu2-slint-spike
```

This does not touch the serial port yet. The next step is to feed the dirty tiles
into a USB display backend once the firmware's partial-window streaming path is
validated.