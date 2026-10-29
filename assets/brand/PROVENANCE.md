# Brand art provenance

`logo.png` and `social-preview.png` were made on 29 October 2026.

The lettering was generated with Codex's native image generation (GPT-Image-2) through
`codex exec --skip-git-repo-check -s workspace-write`, round 1 of 1, four options from one
brief (`r1/PROMPT.txt`). Style reference: the chunky extruded block lettering of the Minecraft
Java Edition title logo.

| Option | Material | Chosen |
|---|---|---|
| `logo_a` | Dark deepslate with glowing cyan veins (memory traces) | Yes |
| `logo_b` | Amethyst crystal blocks with shards | |
| `logo_c` | Copper and stone with redstone circuit lines | |
| `logo_d` | Obsidian with enchantment glints and a pixel brain | |

A won on legibility on both GitHub themes and on meaning: the veins read as stored traces.
The round-1 originals are kept outside git (`r1/*.png` is ignored); their checksums are below.

`logo.png` is option A trimmed to its artwork, scaled to 1600 px wide and quantised with
pngquant. `social-preview.png` (1280 x 640, GitHub's card size) places it on a radial ink
gradient (#12202A to #06090C) with the tagline in SF Pro and the mantra in SF Mono (#4FE3F0),
composed with ImageMagick.

## Checksums (SHA-256)

```
0dcecf9efad93f9b5219e6220dd2dcca99e035cc2297cec6ec4109e1e18b631f r1/logo_a.png
a9b09d543c721b7a3475e238cca4303405927c940cde792b9073d6764e545a25 r1/logo_b.png
ed97d5778a6dc8c8de2bec21e0ab96c77e3286b0f735cc68918a86a7fee09e93 r1/logo_c.png
2cafef5411d1f218e0606543e56376614e0b4476c252fab31ad7dee735ae117d r1/logo_d.png
88012668006a7eaee7f027998c5d3b704e3045f2192f95291e4b3239458e7267 logo.png
16440882d44d3af89bc954160535e53deaa002eb23c87687238a53451a646da3 social-preview.png
```
