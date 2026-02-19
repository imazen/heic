# User Feedback Log

## 2026-02-18: Investigation request for classic-car-iphone12pro.heic dimension swap
User reported decoder outputs 4032x3024 (landscape) but reference PNG is 3024x4032 (portrait). Suspected irot/EXIF rotation issue.

## 2026-02-19: PPS ID mismatch on multilayer001/002/005.heic
User reported "PPS ID mismatch" error on L-HEVC files. Investigated the NAL structure and found root cause: shared mdat data contains NALs for both layer_id=0 (pps_id=0) and layer_id=1 (pps_id=1), but decoder processes all NALs regardless of layer. Fix: filter NAL units by nuh_layer_id.

## 2026-02-19: Investigation request - iphone_rotated_and_mirrored.heic 32.8dB PSNR
User asked to investigate why this file has low PSNR (32.8dB, max diff 33) vs libheif.
