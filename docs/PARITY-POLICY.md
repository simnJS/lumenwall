# Parity policy

How implementers decide when a spec describes a reference behaviour that looks wrong. Applies in this order:

1. **Performance bugs are never reproduced.** Busy loops, per-frame blocking queries, CPU readbacks, polling where events exist: implement the efficient version (see `docs/PERFORMANCE.md`).
2. **Crashes, hangs and fatal errors are fixed.** A missing sound server, D-Bus or file must degrade gracefully, never block or abort.
3. **What the viewer sees and hears matches the reference.** Rendering results, animation timing, audio-reactive values and input mapping are reproduced exactly, including odd formulas, when they change the output. Mark each with a one-line `// parity:` comment.
4. **Gaps versus the public Wallpaper Engine contract are filled.** When the reference simply lacks a feature that Wallpaper Engine documents publicly (web wallpaper JS APIs, SceneScript APIs, particle children…), implement the documented behaviour. This is a superset, not a deviation.
5. **Visible bugs that contradict Wallpaper Engine's documented behaviour** follow Wallpaper Engine when its behaviour is known; otherwise follow the reference and mark `// parity:`.
6. **Frame-rate-dependent behaviour** is reproduced at the reference's frame rate assumptions, but computed from real elapsed time where that gives the same result at the target fps.

Each crate README lists its deliberate deviations under "Deviations".
