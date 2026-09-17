import * as os from 'node:os';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { FlatRunnableSymbol, planRunnables } from './runnablePlanner';
import { TestingIntegration, discoverCli } from './testing';

export function registerRunnableCodeLens(
    context: vscode.ExtensionContext,
    testing: TestingIntegration,
    output: vscode.LogOutputChannel
): vscode.Disposable {
    const changes = new vscode.EventEmitter<void>();
    const provider: vscode.CodeLensProvider = {
        onDidChangeCodeLenses: changes.event,
        async provideCodeLenses(document: vscode.TextDocument): Promise<vscode.CodeLens[]> {
            if (document.languageId !== 'arandu') {
                return [];
            }
            try {
                const symbols = await fetchDocumentSymbols(document);
                const planned = planRunnables(
                    symbols,
                    vscode.SymbolKind.Function,
                    name =>
                        testing.getTestIdForFunction(document.uri, name) !== undefined
                );
                return planned.map(plannedItem => {
                    const range = new vscode.Range(plannedItem.line, 0, plannedItem.line, 0);
                    switch (plannedItem.action) {
                        case 'run':
                            return new vscode.CodeLens(range, {
                                title: '▶ Run',
                                command: 'arandu.runProject',
                                arguments: [document.uri]
                            });
                        case 'check':
                            return new vscode.CodeLens(range, {
                                title: 'Check',
                                command: 'arandu.checkProject',
                                arguments: [document.uri]
                            });
                        case 'test':
                            return new vscode.CodeLens(range, {
                                title: '▶ Run Test',
                                command: 'arandu.runTestAtLine',
                                arguments: [document.uri, plannedItem.name]
                            });
                    }
                });
            } catch {
                return [];
            }
        }
    };
    return vscode.Disposable.from(
        vscode.languages.registerCodeLensProvider({ language: 'arandu' }, provider),
        testing.onDidChangeTests(() => changes.fire()),
        vscode.commands.registerCommand('arandu.runProject', (target: vscode.Uri | undefined) =>
            runCliFromLens(context, target, 'run', output)),
        vscode.commands.registerCommand('arandu.checkProject', (target: vscode.Uri | undefined) =>
            runCliFromLens(context, target, 'build', output)),
        vscode.commands.registerCommand(
            'arandu.runTestAtLine',
            (uri: vscode.Uri | undefined, name: unknown) =>
                runTestFromLens(testing, uri, name, output)
        ),
        changes
    );
}

async function runCliFromLens(
    context: vscode.ExtensionContext,
    target: vscode.Uri | undefined,
    subcommand: 'run' | 'build',
    output: vscode.LogOutputChannel
): Promise<void> {
    const uri = target ?? vscode.window.activeTextEditor?.document.uri;
    if (!uri) {
        return;
    }
    const root = projectRootFor(uri);
    const cli = discoverCli(context, root);
    if (!cli) {
        await vscode.window.showErrorMessage(
            'Could not find the Arandu CLI. Configure arandu.cli.path.'
        );
        return;
    }
    const terminal = vscode.window.createTerminal({
        name: `Arandu ${subcommand}`,
        cwd: root
    });
    terminal.sendText(`${quote(cli)} ${subcommand} ${quote(root)}`);
    terminal.show(true);
    output.info(`${subcommand}: ${cli} ${subcommand} ${root}`);
}

async function runTestFromLens(
    testing: TestingIntegration,
    uri: vscode.Uri | undefined,
    functionName: unknown,
    output: vscode.LogOutputChannel
): Promise<void> {
    if (!(uri instanceof vscode.Uri) || typeof functionName !== 'string') {
        return;
    }
    try {
        const status = await testing.runTestById(uri, functionName);
        const suffix = status === 'passed' || status === 'skipped'
            ? status
            : `${status} — see the Test Explorer for details`;
        vscode.window.showInformationMessage(`Arandu test ${suffix}`);
    } catch (error: unknown) {
        const message = error instanceof Error ? error.message : String(error);
        output.error(`Test execution failed: ${message}`);
        await vscode.window.showErrorMessage(`Arandu test failed: ${message}`);
    }
}

function projectRootFor(uri: vscode.Uri): string {
    const folder = vscode.workspace.getWorkspaceFolder(uri);
    if (folder) {
        return folder.uri.fsPath;
    }
    const fallback = path.dirname(uri.fsPath);
    return fallback === '.' ? os.homedir() : fallback;
}

async function fetchDocumentSymbols(document: vscode.TextDocument): Promise<FlatRunnableSymbol[]> {
    const symbols: unknown = await vscode.commands.executeCommand(
        'vscode.executeDocumentSymbolProvider',
        document.uri
    );
    if (!Array.isArray(symbols)) {
        return [];
    }
    const result: FlatRunnableSymbol[] = [];
    const visit = (value: unknown): void => {
        if (!isRecord(value) || typeof value.kind !== 'number' || typeof value.name !== 'string') {
            return;
        }
        const line = symbolStartLine(value);
        if (line !== undefined) {
            result.push({ name: value.name, kind: value.kind, line });
        }
        const children = value.children;
        if (Array.isArray(children)) {
            for (const child of children) {
                visit(child);
            }
        }
    };
    for (const symbol of symbols) {
        visit(symbol);
    }
    return result;
}

function symbolStartLine(symbol: Record<string, unknown>): number | undefined {
    const range = findRecord(symbol, 'location', 'range');
    if (range) {
        const start = findRecord(range, 'start');
        if (start && typeof start.line === 'number') {
            return start.line;
        }
    }
    const selection = findRecord(symbol, 'selectionRange');
    if (selection) {
        const start = findRecord(selection, 'start');
        if (start && typeof start.line === 'number') {
            return start.line;
        }
    }
    return undefined;
}

function findRecord(root: Record<string, unknown>, ...keys: string[]): Record<string, unknown> | undefined {
    let current: Record<string, unknown> | undefined = root;
    for (const key of keys) {
        const value: unknown = current?.[key];
        current = isRecord(value) ? value : undefined;
    }
    return current;
}

function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function quote(value: string): string {
    if (/^[A-Za-z0-9_./:=@+-]+$/.test(value)) {
        return value;
    }
    const escaped = value.replaceAll('\\', '\\\\').replaceAll('"', '\\"');
    return `"${escaped}"`;
}