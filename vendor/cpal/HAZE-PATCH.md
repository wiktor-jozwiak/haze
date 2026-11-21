# cpal 0.15.3, patched for haze

Upstream crate copied unchanged except `src/host/alsa/mod.rs` and `src/host/wasapi/device.rs`
(lines marked `haze patch`):

1. POLLERR while the PCM is in XRUN is handled as an xrun (recover) instead of a fatal error.
   Before: the worker reported the error and polled again, spinning on POLLERR forever.
2. After an input xrun the capture PCM is `prepare()`d **and** `start()`ed.
   Before: it stayed in PREPARED, the data callback never ran again, the guitar went silent.
3. WASAPI streams open with `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | SRC_DEFAULT_QUALITY`, and a
   format other than the device mix format is no longer rejected up front.
   Before: shared mode accepted only the mix rate, so a 48 kHz engine could not open the
   Katana's 44.1 kHz USB endpoint (`StreamConfigNotSupported`).

Regression test (needs a capture device):
`HAZE_XRUN_DEV=front:CARD=<card>,DEV=0 cargo test -p haze-dsp --test capture_xrun -- --ignored`

Drop this copy once upstream cpal restarts capture after an xrun.
