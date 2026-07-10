# Dashboard Font Subsets

Captured: 2026-07-04

The dashboard ships self-hosted WOFF2 subsets so the public firmware does not request external fonts.

Source CSS request:

```text
https://fonts.googleapis.com/css2?family=Inter:wght@400..700&family=JetBrains+Mono:wght@400;700&family=Barlow+Condensed:wght@600&display=swap
```

Request user agent:

```text
Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36
```

Files:

```text
inter-core-400-700.woff2
https://fonts.gstatic.com/s/inter/v20/UcC73FwrK3iLTeHuS_nVMrMxCp50SjIa1ZL7.woff2
Inter v20, Google Fonts Latin source subset, pyftsubset core UI subset, normal 400-700

jetbrains-mono-core-400-700.woff2
https://fonts.gstatic.com/s/jetbrainsmono/v24/tDbv2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKwBNntkaToggR7BYRbKPxDcwg.woff2
JetBrains Mono v24, Google Fonts Latin source subset, pyftsubset core UI subset, normal 400-700
```

Regeneration:

```text
npm.cmd run fonts:css
```

`scripts/gen-fonts-css.mjs` embeds the WOFF2 bytes into `src/styles/fonts.css` as data URIs and prints raw, base64, and gzip sizes.

Subsetting:

```text
pyftsubset inter-latin-400-700.woff2 --output-file=inter-core-400-700.woff2 --flavor=woff2 --unicodes=U+0020-007E,U+00A0,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215 --layout-features=kern,liga,tnum --desubroutinize
pyftsubset jetbrains-mono-latin-400-700.woff2 --output-file=jetbrains-mono-core-400-700.woff2 --flavor=woff2 --unicodes=U+0020-007E,U+00A0,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215 --layout-features=kern,liga,tnum --desubroutinize
```

Barlow Condensed 600 was tested as `barlow-condensed-core-600.woff2` and cut because the three-face build exceeded the 600 KiB gzip hard gate. Heading stacks use Inter 400-700 for this beta cut.

Licenses:

```text
OFL-Inter.txt
OFL-JetBrainsMono.txt
```
