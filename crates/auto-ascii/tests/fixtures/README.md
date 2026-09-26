`architect-motion.bin` is an ASCI v1 test excerpt, not a golden render.
It contains frames 7580–7729 (zero based, 30 fps) of the supplied
`The Architect.ascii`, reduced to 128×72 so normal workspace tests can
exercise real playback without an external corpus or a video decoder.

Y/E/Ex/Ey use the core area resampler; H and RGB565 C use nearest samples
(C is 64×36). Frame timing, aspect ratio, shot levels and cut boundaries
are retained, rebased to frame zero. The container uses temporal deltas,
a keyframe interval of 60, and zstd level 9. Metadata records the source
and frame range. No glyph, palette, codec output or expected result is baked in.

`ascii_temporal.rs` plays 121 frames at five zoom sizes. At the last frame,
any-cell differences from cold measure 17.2–20.9%; the limit is 22%.
Background/default-background differences measure 0.19–1.83%; the limit is
3%. The original 3cef5ec codec fails the first size with 44.7% any-cell
and 30.4% background differences. Glyph differences remain intentional:
colour follows current input while glyph/edge/half hysteresis limits flicker.
The final 90 frames also enforce glyph switches per cell ≤1.2× pixels.

Extraction source, full-size four-clip comparisons, before/after renders,
and logs are in the accompanying `compare/ascii-zoom/reproduce/` artifacts.
