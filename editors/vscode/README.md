# PHPScript for VS Code

Syntax highlighting for [PHPScript](https://github.com/bimsonz/elephpant) — PHP-flavored TypeScript. Files with the `.psx` extension light up: keywords, variables, types, strings (including `$var` and `{$expr}` interpolation), comments, and operators all get the colours your theme expects.

## Install

From the VS Code Marketplace:

```bash
code --install-extension bimsonz.psx-vscode
```

Or open the Extensions panel and search for "PHPScript".

## Develop locally

```bash
cd editors/vscode
code .
# F5 to open a fresh VS Code window with the extension loaded.
# Open any .psx file in the new window to see highlighting.
```

## What's in scope

This extension is a pure TextMate grammar — keyword/operator/identifier highlighting only. No language-server features (diagnostics, hover, goto-definition) yet — those land in a follow-up `psx-lsp` extension.

## License

MIT.
