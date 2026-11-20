<?php

// spc patch script (`spc build --with-added-patch`), used for every build.
//
// Right after extracting the sources, spc applies phpmicro's patches
// (SPC_MICRO_PATCHES) to php-src, micro built or not, each in the file for
// the newest PHP version spc knows up to the one being built. spc 2.8 knows
// versions up to 8.5 only, and for 8.6 falls back to the 8.0 files, which
// no longer apply. So build.sh turns spc's own patching off
// (SPC_MICRO_PATCHES=) and this applies, at the same point, the patches that
// change the cli and fpm we build, each in phpmicro's newest file up to the
// PHP version being built:
//   disable_huge_page  (Linux)  no 2 MiB segment alignment probe in configure
//   macos_iconv        (macOS)  configure finds iconv in the static libiconv
// The others spc applies by default (cli_checks) only let the micro SAPI
// pass php-src's checks for cli, and we never build micro.

declare(strict_types=1);

use SPC\store\SourcePatcher;

if (patch_point() !== 'after-exts-extract') {
    return;
}

$names = match (PHP_OS_FAMILY) {
    'Linux' => ['disable_huge_page'],
    'Darwin' => ['macos_iconv'],
    default => throw new RuntimeException('micro-patches.php: unsupported OS ' . PHP_OS_FAMILY),
};
$php_src = SOURCE_PATH . '/php-src';
$dir = "{$php_src}/sapi/micro/patches";
$id = builder()->getPHPVersionID();
$version = intdiv($id, 10000) * 10 + intdiv($id % 10000, 100); // 80600 -> 86

foreach ($names as $name) {
    $best = null;
    foreach (glob("{$dir}/{$name}_*.patch") ?: [] as $file) {
        if (preg_match('/_(\d{2})\.patch$/', $file, $m) && (int) $m[1] <= $version
            && ($best === null || (int) $m[1] > $best[0])) {
            $best = [(int) $m[1], $file];
        }
    }
    $file = is_file("{$dir}/{$name}.patch") ? "{$dir}/{$name}.patch" : ($best[1] ?? null);
    if ($file === null) {
        throw new RuntimeException("micro-patches.php: phpmicro has no {$name} patch for PHP {$version}");
    }
    logger()->info('Patching php-src with sapi/micro/patches/' . basename($file));
    SourcePatcher::patchFile($file, $php_src);
}
