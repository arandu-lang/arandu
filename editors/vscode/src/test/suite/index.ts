import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';

// A cold Windows Extension Host may spend more than ten seconds creating its
// log channel and activating installed extensions. Polling still returns as
// soon as the requested product evidence appears; this is only the hard E2E
// failure bound, not an unconditional delay.
const TIMEOUT_MS = 30_000;

interface AranduExtensionApi {
    getRuntimeState(): { state: string; observedCrashCount: number };
    getDiscoveredTestCount(): number;
    testRunFirstDiscovered(): Promise<string>;
    testCrashServer(): Promise<void>;
}

export async function run(): Promise<void> {
    const serverPath = process.env.ARANDU_LSP_TEST_PATH;
    assert.ok(serverPath, 'ARANDU_LSP_TEST_PATH must point to the test server');
    assert.ok(fs.existsSync(serverPath), `arandu-lsp test binary missing: ${serverPath}`);

    const configuration = vscode.workspace.getConfiguration('arandu');
    await configuration.update('server.path', serverPath, vscode.ConfigurationTarget.Global);
    const cliPath = process.env.ARANDU_CLI_TEST_PATH;
    if (cliPath) {
        assert.ok(fs.existsSync(cliPath), `arandu CLI test binary missing: ${cliPath}`);
        await configuration.update('cli.path', cliPath, vscode.ConfigurationTarget.Global);
    }
    try {
        const workspace = vscode.workspace.workspaceFolders?.[0];
        assert.ok(workspace, 'Extension Host test requires a workspace');

        const extension = vscode.extensions.getExtension('arandu.arandu-lang');
        assert.ok(extension, 'Arandu extension was not installed in the Extension Host');
        if (process.env.ARANDU_EXPECT_INSTALLED_EXTENSION === '1') {
            const installedRoot = process.env.ARANDU_INSTALLED_EXTENSIONS_ROOT;
            assert.ok(installedRoot, 'installed extension root was not provided by the harness');
            const relativeExtensionPath = path.relative(installedRoot, extension.extensionPath);
            assert.equal(
                relativeExtensionPath !== ''
                    && !relativeExtensionPath.startsWith(`..${path.sep}`)
                    && relativeExtensionPath !== '..'
                    && !path.isAbsolute(relativeExtensionPath),
                true,
                `test loaded a development extension instead of the installed VSIX: ${extension.extensionPath}`
            );
        }
        await extension.activate();
        const api = extension.exports as AranduExtensionApi;
        await poll(() => api.getRuntimeState().state === 'ready' ? true : undefined);

        const commands = await vscode.commands.getCommands(true);
        assert.ok(commands.includes('arandu.restartServer'));
        assert.ok(commands.includes('arandu.showServerLogs'));
        assert.ok(commands.includes('arandu.refreshTests'));
        assert.ok(commands.includes('arandu.runBenchmark'));
        await poll(() => api.getDiscoveredTestCount() > 0 ? true : undefined);
        assert.equal(await api.testRunFirstDiscovered(), 'passed');

        const uri = vscode.Uri.joinPath(workspace.uri, 'main.aru');
        const document = await vscode.workspace.openTextDocument(uri);
        await vscode.window.showTextDocument(document);
        const diagnostics = await poll(() => {
            const current = vscode.languages.getDiagnostics(uri);
            return current.some(diagnostic => diagnosticCode(diagnostic) === 'N001') ? current : undefined;
        });
        assert.ok(diagnostics.some(diagnostic => diagnosticCode(diagnostic) === 'N001'));
        const unresolved = diagnostics.find(diagnostic => diagnosticCode(diagnostic) === 'N001');
        assert.ok(unresolved, 'Problems must retain the compiler diagnostic code');
        assert.equal(unresolved.source, 'arandu');
        assert.ok(unresolved.range.end.isAfter(unresolved.range.start));

        const runnableTitles = await poll(async () => {
            const lenses = await vscode.commands.executeCommand<readonly vscode.CodeLens[]>(
                'vscode.executeCodeLensProvider',
                uri
            );
            const titles = (lenses ?? []).flatMap(lens =>
                lens.command?.title ? [lens.command.title] : []);
            return titles.includes('▶ Run') && titles.includes('Check') ? titles : undefined;
        });
        assert.ok(runnableTitles.includes('▶ Run'), 'main must expose a Run code lens');
        assert.ok(runnableTitles.includes('Check'), 'main must expose a Check code lens');

        const testUri = vscode.Uri.joinPath(workspace.uri, 'tests', 'smoke.aru');
        const testDocument = await vscode.workspace.openTextDocument(testUri);
        await vscode.window.showTextDocument(testDocument);
        const testTitles = await poll(async () => {
            const lenses = await vscode.commands.executeCommand<readonly vscode.CodeLens[]>(
                'vscode.executeCodeLensProvider',
                testUri
            );
            const titles = (lenses ?? []).flatMap(lens =>
                lens.command?.title ? [lens.command.title] : []);
            return titles.includes('▶ Run Test') ? titles : undefined;
        });
        assert.ok(testTitles.includes('▶ Run Test'), 'decorated tests must expose a Run Test lens');
        await vscode.commands.executeCommand('workbench.action.closeAllEditors');
        await vscode.window.showTextDocument(document);

        const completionUri = vscode.Uri.joinPath(workspace.uri, 'completion.aru');
        const completionDocument = await vscode.workspace.openTextDocument(completionUri);
        await vscode.window.showTextDocument(completionDocument);
        const completionPosition = new vscode.Position(0, 2);
        const completions = await poll(async () => {
            const result = await vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                completionUri,
                completionPosition
            );
            return result?.items.some(item => completionItemLabel(item) === 'func')
                ? result
                : undefined;
        });
        assert.ok(completions.items.some(item => completionItemLabel(item) === 'func'));

        await verifyNavigationAndRename(workspace);

        const formatUri = vscode.Uri.joinPath(workspace.uri, 'format.aru');
        const formatDocument = await vscode.workspace.openTextDocument(formatUri);
        await vscode.window.showTextDocument(formatDocument);
        const formatOptions = { tabSize: 2, insertSpaces: false };
        const formatEdits = await poll(() =>
            vscode.commands.executeCommand<vscode.TextEdit[]>(
                'vscode.executeFormatDocumentProvider',
                formatUri,
                formatOptions
            ).then(edits => edits && edits.length > 0 ? edits : undefined)
        );
        assert.ok(formatEdits.length > 0);
        assert.ok(formatEdits.some(edit => edit.range.start.line === 1 && edit.range.end.line === 1));
        assert.ok(formatEdits.every(edit => edit.range.start.line > 0), 'unchanged header must not be replaced');
        const workspaceEdit = new vscode.WorkspaceEdit();
        workspaceEdit.set(formatUri, formatEdits);
        assert.equal(await vscode.workspace.applyEdit(workspaceEdit), true);
        assert.equal(formatDocument.getText(), 'func main(): int {\n    return 1\n}\n');
        const stableEdits = await vscode.commands.executeCommand<vscode.TextEdit[]>(
            'vscode.executeFormatDocumentProvider',
            formatUri,
            formatOptions
        );
        assert.ok(stableEdits === undefined || stableEdits.length === 0);

        await verifyHighlightingAcrossBuiltInThemes(uri);

        await vscode.commands.executeCommand('arandu.restartServer');
        const afterRestart = await poll(() =>
            vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                completionUri,
                completionPosition
            ).then(result => result?.items.some(item => completionItemLabel(item) === 'func')
                ? result
                : undefined)
        );
        assert.ok(afterRestart.items.some(item => completionItemLabel(item) === 'func'));

        const crashesBefore = api.getRuntimeState().observedCrashCount;
        await api.testCrashServer();
        await poll(() => api.getRuntimeState().observedCrashCount > crashesBefore ? true : undefined);
        await poll(() => api.getRuntimeState().state === 'ready' ? true : undefined);
        const afterCrashRecovery = await poll(() =>
            vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                completionUri,
                completionPosition
            ).then(result => result?.items.some(item => completionItemLabel(item) === 'func')
                ? result
                : undefined)
        );
        assert.ok(
            afterCrashRecovery.items.some(item => completionItemLabel(item) === 'func'),
            'Arandu completion must recover after a real server crash'
        );
    } finally {
        await configuration.update('server.path', undefined, vscode.ConfigurationTarget.Global);
        await configuration.update('cli.path', undefined, vscode.ConfigurationTarget.Global);
        await vscode.commands.executeCommand('workbench.action.closeAllEditors');
    }
}

async function verifyNavigationAndRename(workspace: vscode.WorkspaceFolder): Promise<void> {
    const uri = vscode.Uri.joinPath(workspace.uri, 'navigation.aru');
    const document = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(document);
    const callLine = document.lineAt(5).text;
    const callColumn = callLine.indexOf('add');
    assert.ok(callColumn >= 0, 'navigation fixture must contain the add call');
    const callPosition = new vscode.Position(5, callColumn + 1);

    const definitions = await poll(() =>
        vscode.commands.executeCommand<Array<vscode.Location | vscode.LocationLink>>(
            'vscode.executeDefinitionProvider',
            uri,
            callPosition
        ).then(result => result && result.length > 0 ? result : undefined)
    );
    const definition = definitions[0];
    assert.ok(definition);
    const definitionUri = definition instanceof vscode.Location
        ? definition.uri
        : definition.targetUri;
    assert.equal(definitionUri.toString(), uri.toString());

    const references = await vscode.commands.executeCommand<vscode.Location[]>(
        'vscode.executeReferenceProvider',
        uri,
        callPosition
    );
    assert.ok(Array.isArray(references), 'references provider must return a structured result');

    const symbols = await poll(() =>
        vscode.commands.executeCommand<Array<vscode.DocumentSymbol | vscode.SymbolInformation>>(
            'vscode.executeDocumentSymbolProvider',
            uri
        ).then(result => result && result.length >= 2 ? result : undefined)
    );
    assert.ok(symbols.some(symbol => symbol.name === 'add'));
    assert.ok(symbols.some(symbol => symbol.name === 'main'));

    const rename = await poll(() =>
        vscode.commands.executeCommand<vscode.WorkspaceEdit>(
            'vscode.executeDocumentRenameProvider',
            uri,
            callPosition,
            'sum'
        )
    );
    const renameEdits = rename.entries()
        .filter(([editUri]) => editUri.toString() === uri.toString())
        .flatMap(([, edits]) => edits);
    assert.equal(renameEdits.length, 2);
    assert.ok(renameEdits.every(edit => edit.newText === 'sum'));
}

function diagnosticCode(diagnostic: vscode.Diagnostic): string | number | undefined {
    const code = diagnostic.code;
    return typeof code === 'object' ? code.value : code;
}

function completionItemLabel(item: vscode.CompletionItem): string {
    const label = item.label;
    return typeof label === 'string' ? label : label.label;
}

async function verifyHighlightingAcrossBuiltInThemes(uri: vscode.Uri): Promise<void> {
    const workbench = vscode.workspace.getConfiguration('workbench');
    const previousTheme = workbench.get<string>('colorTheme');
    const legend = await poll(() =>
        vscode.commands.executeCommand<vscode.SemanticTokensLegend>(
            'vscode.provideDocumentSemanticTokensLegend',
            uri
        )
    );
    assert.ok(legend.tokenTypes.includes('function'));
    assert.ok(legend.tokenTypes.includes('variable'));

    let baseline: number[] | undefined;
    const themes: ReadonlyArray<readonly [string, vscode.ColorThemeKind]> = [
        ['Default Dark+', vscode.ColorThemeKind.Dark],
        ['Default Light+', vscode.ColorThemeKind.Light],
        ['Default High Contrast', vscode.ColorThemeKind.HighContrast]
    ];
    try {
        for (const [theme, expectedKind] of themes) {
            await workbench.update('colorTheme', theme, vscode.ConfigurationTarget.Global);
            await poll(() => vscode.window.activeColorTheme.kind === expectedKind ? true : undefined);
            const tokens = await poll(() =>
                vscode.commands.executeCommand<vscode.SemanticTokens>(
                    'vscode.provideDocumentSemanticTokens',
                    uri
                ).then(value => value.data.length > 0 ? value : undefined)
            );
            assert.equal(tokens.data.length % 5, 0);
            const data = Array.from(tokens.data);
            if (baseline === undefined) {
                baseline = data;
            } else {
                assert.deepEqual(data, baseline, `semantic classification changed under ${theme}`);
            }
        }
    } finally {
        await workbench.update('colorTheme', previousTheme, vscode.ConfigurationTarget.Global);
    }
}

async function poll<T>(operation: () => T | undefined | PromiseLike<T | undefined>): Promise<T> {
    const deadline = Date.now() + TIMEOUT_MS;
    let lastError: unknown;
    while (Date.now() < deadline) {
        try {
            const result = await operation();
            if (result !== undefined) {
                return result;
            }
        } catch (error: unknown) {
            lastError = error;
        }
        await new Promise(resolve => setTimeout(resolve, 50));
    }
    const detail = lastError instanceof Error ? `: ${lastError.message}` : '';
    throw new Error(`Extension Host operation timed out${detail}`);
}
