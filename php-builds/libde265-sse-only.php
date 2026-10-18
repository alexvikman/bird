<?php

// spc patch script (`spc build --with-added-patch`), used for linux-x86_64 only.
//
// libde265 1.1.1+ picks its AVX2 / AVX-512 kernels at runtime with
// __builtin_cpu_supports, which needs __cpu_model and __cpu_indicator_init from
// libgcc. zig's compiler-rt has neither, so linking ImageMagick's utilities
// (and PHP) against libde265.a fails with undefined symbols. Build libde265
// with its SSE4.1 kernels only; HEIC decoding works the same, a little slower.

declare(strict_types=1);

if (patch_point() !== 'before-library[libde265]-build') {
    return;
}

$file = builder()->getLib('libde265')->getSourceDir() . '/CMakeLists.txt';
$cmake = file_get_contents($file);
if ($cmake === false) {
    throw new RuntimeException("cannot read {$file}");
}
$option = '/^(option\((ENABLE_AVX2|ENABLE_AVX512)\s+"[^"]*"\s+)(ON|OFF)\)/m';
$cmake = preg_replace($option, '$1OFF)', $cmake, -1, $count);
// Both options must exist: a libde265 that renames them would bring the
// undefined symbols back, so fail here instead of at the link.
if ($count !== 2) {
    throw new RuntimeException("{$file}: expected options ENABLE_AVX2 and ENABLE_AVX512, found {$count}");
}
file_put_contents($file, $cmake);
logger()->info('libde265: AVX2 and AVX-512 kernels disabled (zig has no __cpu_model)');
