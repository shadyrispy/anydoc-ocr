#!/usr/bin/env bash
# Reusable runtime env for anydoc-ocr (x86_64). Sources real lib locations.
export ORT_LIB_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/lib
export ORT_INCLUDE_LOCATION=/workspace/anydoc-ocr/third_party/ort/x64/onnxruntime-linux-x64-1.20.1/include
export ORT_PREFER_DYNAMIC_LINK=1
export PDFIUM_LIB_DIR=/workspace/anydoc-ocr/third_party/pdfium/x64/lib
export OAR_HOME=/root/.oar
export LD_LIBRARY_PATH="${ORT_LIB_LOCATION}:${PDFIUM_LIB_DIR}:${LD_LIBRARY_PATH}"
exec /workspace/anydoc-ocr/target/release/anydoc-ocr "$@"
