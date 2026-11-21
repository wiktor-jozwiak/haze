# cpal 0.15.3, patched for haze

Upstream crate copied unchanged except `src/host/alsa/mod.rs` (lines marked `haze patch`):

1. POLLERR while the PCM is in XRUN is handled as an xrun (recover) instead of a fatal error.
   Before: the worker reported the error and polled again, spinning on POLLERR forever.
2. After an input xrun the capture PCM is `prepare()`d **and** `start()`ed.
   Before: it stayed in PREPARED, the data callback never ran again, the guitar went silent.

Regression test (needs a capture device):
`HAZE_XRUN_DEV=front:CARD=<card>,DEV=0 cargo test -p haze-dsp --test capture_xrun -- --ignored`

Drop this copy once upstream cpal restarts capture after an xrun.
