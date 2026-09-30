# crates/lcu/data —— 编译期资源

这里存放 `include_str!` 进 lcu 二进制的数据文件, 因此本目录被 `.gitignore`
显式豁免(该文件的 `!crates/lcu/data/` 行)。删错任何文件都会让干净检出
编译失败, 处理前想清楚。

| 文件 | 用途 | 消费方 |
| --- | --- | --- |
| `counter_runes.default.toml` | counter 符文默认规则库(用户可在 %APPDATA%\champr\counter-runes.toml 覆盖) | `counter.rs` |
| `rune_lattice.json` | 62 枚符文 id → (tree, row, 中文名)的格子图, 校验规则副系合法性 + 渲染符文名 | `counter.rs::RuneLattice` |
| `runesReforged-16.19.1.json` | 上面格子图的 DDragon 原始快照(版本 16.19.1, zh_CN), 保留作溯源材料 | 仅生成用 |

## 升级格子图(拳头大版本改了符文树时)

```powershell
# 1) 更新原始快照(改版本号)
(New-Object System.Net.WebClient).DownloadFile(
  'https://ddragon.leagueoflegends.com/cdn/<ver>/data/zh_CN/runesReforged.json',
  'crates\lcu\data\runesReforged-<ver>.json')

# 2) 重生成格子图(不要带 BOM — serde_json 会挂; 下述写法是无 BOM 的)
$raw = Get-Content -Raw -Encoding UTF8 crates\lcu\data\runesReforged-<ver>.json
$styles = $raw | ConvertFrom-Json
$out = @(); foreach ($style in $styles) {
  for ($row=0; $row -lt $style.slots.Count; $row++) {
    foreach ($rune in $style.slots[$row].runes) {
      $out += [pscustomobject]@{ id=$rune.id; tree=$style.id; row=$row; name=$rune.name }
    }
  }
}
[System.IO.File]::WriteAllText(
  "$PWD\crates\lcu\data\rune_lattice.json",
  ($out | ConvertTo-Json -Compress),
  (New-Object System.Text.UTF8Encoding($false)))
```

改完后 `cargo test -p lcu counter` 会立刻暴露默认规则里 id 是否还成立。

⚠ 碎片槽(5001/5002/5003 等)不在 runesReforged.json 里, 它们的合法性校验是
`RuneLattice::defense_shard_ok` 内置白名单 —— 拳头改了碎片系统要同步改那里。

⚠ 若格子图被外部编辑器重存为带 BOM UTF-8 (常见 Windows 杀毒/Notepad 行为),
`RuneLattice::builtin` 已做 BOM 剥离容错, 但生成脚本默认就不该产出 BOM。
