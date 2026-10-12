# gen-icons

`cargo run --release -p gen-icons` — renders the icon (SPEC ch.0 §1.2) from one geometry definition into
`assets/icon/`: `fennec.svg`, `fennec-{16,20,24,32,48,64,128,256,512}.png`, `fennec.ico`, and the tray variants
(`tray`, `tray-white`, `tray-indexing`, `tray-paused`; 16/32 px). Small sizes get thicker strokes.
