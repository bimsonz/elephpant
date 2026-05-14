// VS Code extension entry. Activates on `.psx` files; spawns the psx-lsp
// binary and routes language-server requests through it.

import * as path from "node:path";
import * as fs from "node:fs";
import { workspace, ExtensionContext, window } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: ExtensionContext): void {
  const serverPath = resolveServerPath(context);
  if (!serverPath) {
    window.showWarningMessage(
      "psx-lsp binary not found. Syntax highlighting still works; install psx-cli to enable diagnostics + navigation.",
    );
    return;
  }

  const serverOptions: ServerOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "psx" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.psx"),
    },
  };

  client = new LanguageClient(
    "psx",
    "PHPScript Language Server",
    serverOptions,
    clientOptions,
  );

  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}

/**
 * Resolve the psx-lsp binary, preferring (in order):
 *   1. `psx.serverPath` setting if present and the file exists.
 *   2. A binary bundled with the extension under `bin/<platform>/psx-lsp`.
 *   3. `psx-lsp` on $PATH (handled by spawning by name).
 */
function resolveServerPath(context: ExtensionContext): string | undefined {
  const configured = workspace.getConfiguration("psx").get<string>("serverPath");
  if (configured && fs.existsSync(configured)) {
    return configured;
  }
  const platform = process.platform;
  const arch = process.arch;
  const ext = platform === "win32" ? ".exe" : "";
  const bundled = path.join(
    context.extensionPath,
    "bin",
    `${platform}-${arch}`,
    `psx-lsp${ext}`,
  );
  if (fs.existsSync(bundled)) {
    return bundled;
  }
  // Fall back to PATH — VS Code's child_process resolution will handle it.
  return `psx-lsp${ext}`;
}
