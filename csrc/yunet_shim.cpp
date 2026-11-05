// C ABI wrapper around libfacedetection's (C++-linkage) entry point.
#include "facedetectcnn.h"

extern "C" int* yunet_detect(unsigned char* result_buffer, unsigned char* bgr, int width, int height, int step) {
    return facedetect_cnn(result_buffer, bgr, width, height, step);
}
