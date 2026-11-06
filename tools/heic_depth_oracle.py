#!/usr/bin/env python3
"""Read-only libheif oracle for embedded depth; never used by product code.

Prints file identity, display dimensions, representation and SHA-256 of
row-packed little-endian u16 sample codes. Does not write an image.
"""
import ctypes as c
import hashlib
import json
import sys
from pathlib import Path


class Error(c.Structure):
    _fields_ = [('code', c.c_int), ('subcode', c.c_int), ('message', c.c_char_p)]


class DepthInfo(c.Structure):
    _fields_ = [('version', c.c_uint8), ('has_z_near', c.c_uint8), ('has_z_far', c.c_uint8),
                ('has_d_min', c.c_uint8), ('has_d_max', c.c_uint8),
                ('z_near', c.c_double), ('z_far', c.c_double), ('d_min', c.c_double), ('d_max', c.c_double),
                ('representation_type', c.c_int), ('disparity_reference_view', c.c_uint32),
                ('model_size', c.c_uint32), ('model', c.c_void_p)]


def main():
    path = Path(sys.argv[1])
    lib = c.CDLL('libheif.so.1')
    ptr = c.c_void_p
    def bind(name, result, *args):
        f = getattr(lib, 'heif_' + name)
        f.restype, f.argtypes = result, args
        return f
    def check(error):
        if error.code:
            raise RuntimeError((error.code, error.subcode, error.message.decode()))
    version = bind('get_version', c.c_char_p)().decode()
    context = bind('context_alloc', ptr)()
    primary, depth, image = ptr(), ptr(), ptr()
    info = c.POINTER(DepthInfo)()
    try:
        check(bind('context_read_from_file', Error, ptr, c.c_char_p, ptr)(context, bytes(path), None))
        check(bind('context_get_primary_image_handle', Error, ptr, c.POINTER(ptr))(context, c.byref(primary)))
        count = bind('image_handle_get_number_of_depth_images', c.c_int, ptr)(primary)
        if count != 1:
            raise RuntimeError(f'expected one depth item, found {count}')
        ids = (c.c_uint32 * count)()
        assert bind('image_handle_get_list_of_depth_image_IDs', c.c_int, ptr, c.POINTER(c.c_uint32), c.c_int)(primary, ids, count) == count
        check(bind('image_handle_get_depth_image_handle', Error, ptr, c.c_uint32, c.POINTER(ptr))(primary, ids[0], c.byref(depth)))
        assert bind('image_handle_get_depth_image_representation_info', c.c_int, ptr, c.c_uint32, c.POINTER(c.POINTER(DepthInfo)))(primary, ids[0], c.byref(info))
        check(bind('decode_image', Error, ptr, c.POINTER(ptr), c.c_int, c.c_int, ptr)(depth, c.byref(image), 2, 0, None))
        width = bind('image_get_width', c.c_int, ptr, c.c_int)(image, 0)
        height = bind('image_get_height', c.c_int, ptr, c.c_int)(image, 0)
        bits = bind('image_get_bits_per_pixel_range', c.c_int, ptr, c.c_int)(image, 0)
        stride = c.c_size_t()
        plane = bind('image_get_plane_readonly2', ptr, ptr, c.c_int, c.POINTER(c.c_size_t))(image, 0, c.byref(stride))
        if not plane or width <= 0 or height <= 0 or not 1 <= bits <= 16:
            raise RuntimeError('invalid decoded depth plane')
        raw = bytearray()
        for y in range(height):
            row = c.string_at(plane + y * stride.value, width * (1 if bits <= 8 else 2))
            if bits <= 8:
                for value in row:
                    raw += value.to_bytes(2, 'little')
            elif sys.byteorder == 'little':
                raw += row
            else:
                for i in range(0, len(row), 2):
                    raw += int.from_bytes(row[i:i+2], sys.byteorder).to_bytes(2, 'little')
        if len(sys.argv) == 3:
            with Path(sys.argv[2]).open('xb') as output:
                output.write(raw)
        print(json.dumps({'oracle': 'libheif', 'version': version, 'file_sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
                          'width': width, 'height': height, 'bit_depth': bits,
                          'representation_type': info.contents.representation_type,
                          'range': {name: getattr(info.contents, name) if getattr(info.contents, 'has_' + name) else None for name in ('z_near', 'z_far', 'd_min', 'd_max')},
                          'u16_le_sha256': hashlib.sha256(raw).hexdigest()}, indent=2))
    finally:
        if image: bind('image_release', None, ptr)(image)
        if info: bind('depth_representation_info_free', None, c.POINTER(DepthInfo))(info)
        for handle in (depth, primary):
            if handle: bind('image_handle_release', None, ptr)(handle)
        bind('context_free', None, ptr)(context)


if __name__ == '__main__':
    main()
