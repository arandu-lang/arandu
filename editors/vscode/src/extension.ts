import * as vscode from 'vscode';
import {
    CloseAction,
    ErrorAction,
    ErrorHandler,
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    State,
    TransportKind
} from 'vscode-languageclient/node';
import { discoverServer } from './serverDiscovery';
import { CrashRestartPolicy } from './serverLifecycle';
import { registerRunnableCodeLens } from './runnableLens';
import { checkServerVersion } from './serverVersion';
import { TestingIntegration, createTestingIntegration } from './testing';

const STOP_TIMEOUT_MS = 2_000;

let client: LanguageClient | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let traceOutputChannel: vscode.LogOutputChannel | undefined;
let clientStateSubscription: vscode.Disposable | undefined;
let lifecycle = Promise.resolve();
let deactivating = false;
let runtimeState: ServerUiState = 'stopped';
let observedCrashCount = 0;
let testingIntegration: TestingIntegration | undefined;
let singleFileNoticeShown = false;

type ServerUiState = 'starting' | 'indexing' | 'single-file' | 'ready' | 'restarting' | 'missing' | 'stopped';

interface RuntimeState {
    readonly state: ServerUiState;
    readonly observedCrashCount: number;
}

interface AranduExtensionApi {
    getRuntimeState(): RuntimeState;
    getDiscoveredTestCount(): number;
    testRunFirstDiscovered(): Promise<string>;
    testCrashServer(): Promise<void>;
}

function getRuntimeState(): RuntimeState {
    return { state: runtimeState, observedCrashCount };
}

function getDiscoveredTestCount(): number {
    return testingIntegration?.getDiscoveredCount() ?? 0;
}

async function testCrashServer(): Promise<void> {
    if (process.env.ARANDU_LSP_TEST_ALLOW_CRASH !== '1' || !client) {
        throw new Error('The crash hook is available only in the Extension Host recovery test');
    }
    await client.sendNotification('arandu/testCrash');
}

async function testRunFirstDiscovered(): Promise<string> {
    if (process.env.ARANDU_LSP_TEST_ALLOW_CRASH !== '1' || !testingIntegration) {
        throw new Error('The execution hook is available only in Extension Host tests');
    }
    return testingIntegration.runFirstForTest();
}

export async function activate(context: vscode.ExtensionContext): Promise<AranduExtensionApi | undefined> {
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    statusBarItem.command = 'arandu.showServerLogs';
    statusBarItem.accessibilityInformation = { label: 'Arandu language server status' };
    context.subscriptions.push(statusBarItem);

    traceOutputChannel = vscode.window.createOutputChannel('Arandu Language Server', { log: true });
    context.subscriptions.push(traceOutputChannel);

    const fileWatcher = vscode.workspace.createFileSystemWatcher('**/{*.aru,arandu.toml,Arandu.toml}');
    context.subscriptions.push(fileWatcher);

    context.subscriptions.push(
        vscode.commands.registerCommand('arandu.showServerLogs', () => {
            traceOutputChannel?.show(true);
        }),
        vscode.commands.registerCommand('arandu.restartServer', () => restartLanguageServer(context, fileWatcher))
    );
    context.subscriptions.push(vscode.commands.registerCommand('arandu.initializePackage', () => {
        const folder = vscode.workspace.workspaceFolders?.find(candidate => candidate.uri.scheme === 'file');
        if (!folder) {
            return;
        }
        const terminal = vscode.window.createTerminal({ name: 'Arandu: Initialize Package', cwd: folder.uri.fsPath });
        terminal.show();
        terminal.sendText('arandu_cli init', false);
    }));

    testingIntegration = createTestingIntegration(context, fileWatcher, traceOutputChannel);
    context.subscriptions.push(testingIntegration);
    context.subscriptions.push(
        registerRunnableCodeLens(context, testingIntegration, traceOutputChannel)
    );

    await restartLanguageServer(context, fileWatcher);
    return process.env.ARANDU_LSP_TEST_ALLOW_CRASH === '1'
        ? { getRuntimeState, getDiscoveredTestCount, testRunFirstDiscovered, testCrashServer }
        : undefined;
}

export async function deactivate(): Promise<void> {
    deactivating = true;
    await lifecycle;
    await stopClient();
}

function restartLanguageServer(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): Promise<void> {
    lifecycle = lifecycle.catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        traceOutputChannel?.error(`Previous server lifecycle operation failed: ${message}`);
    }).then(async () => {
        await stopClient();
        await startLanguageServer(context, fileWatcher);
    });
    return lifecycle;
}

async function startLanguageServer(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): Promise<void> {
    setStatus('starting');
    const configuration = vscode.workspace.getConfiguration('arandu');
    const result = discoverServer({
        configuredPath: configuration.get<string | null>('server.path'),
        workspaceRoots: (vscode.workspace.workspaceFolders ?? [])
            .filter(folder => folder.uri.scheme === 'file')
            .map(folder => folder.uri.fsPath),
        extensionPath: context.extensionPath
    });
    if (!result.resolution) {
        traceOutputChannel?.error(result.failure.message);
        for (const candidate of result.failure.checked) {
            traceOutputChannel?.debug(`Checked ${candidate}`);
        }
        await reportMissingServer(result.failure.message);
        return;
    }

    const version = await checkServerVersion(result.resolution.command);
    if (!version.ok) {
        await reportMissingServer(
            `The Arandu Language Server found at ${result.resolution.command} (via ${result.resolution.source}) does not respond to --version. The binary may be damaged or not be arandu-lsp.`
        );
        return;
    }
    traceOutputChannel?.info(
        `Starting Arandu Language Server ${version.output.trim()} (discovered via ${result.resolution.source})`
    );
    const packageInfo = context.extension.packageJSON as { version?: unknown };
    const extensionVersion = typeof packageInfo.version === 'string' ? packageInfo.version : 'unknown';
    traceOutputChannel?.info(`Arandu extension ${extensionVersion}`);
    const roots = (vscode.workspace.workspaceFolders ?? []).map(folder => folder.uri.toString(true));
    traceOutputChannel?.info(`Workspace roots: ${roots.length === 0 ? '(none)' : roots.join(', ')}`);
    const crashPolicy = new CrashRestartPolicy();
    const errorHandler: ErrorHandler = {
        error(error, _message, count) {
            traceOutputChannel?.error(
                `Language Server Protocol transport error${count === undefined ? '' : ` #${count}`}: ${error.message}`
            );
            return { action: count !== undefined && count <= 3 ? ErrorAction.Continue : ErrorAction.Shutdown };
        },
        closed() {
            if (deactivating) {
                return { action: CloseAction.DoNotRestart, handled: true };
            }
            const decision = crashPolicy.recordCrash(Date.now());
            observedCrashCount = decision.crashCount;
            if (decision.restart) {
                const detail = `Arandu Language Server crashed; restarting automatically (${decision.crashCount}/3).`;
                traceOutputChannel?.warn(detail);
                setStatus('restarting', detail);
                return { action: CloseAction.Restart, handled: true };
            }
            const detail = 'Arandu Language Server repeatedly crashed and automatic restart was stopped.';
            traceOutputChannel?.error(`${detail} Use “Arandu: Restart Language Server” after checking the logs.`);
            setStatus('stopped', detail);
            promptAfterCrashLoop(context, fileWatcher);
            return { action: CloseAction.DoNotRestart, handled: true };
        }
    };
    const serverOptions: ServerOptions = {
        run: { command: result.resolution.command, transport: TransportKind.stdio },
        debug: { command: result.resolution.command, transport: TransportKind.stdio }
    };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ scheme: 'file', language: 'arandu' }],
        synchronize: { fileEvents: fileWatcher },
        traceOutputChannel,
        errorHandler
    };
    const nextClient = new LanguageClient(
        'aranduLanguageServer',
        'Arandu Language Server',
        serverOptions,
        clientOptions
    );
    nextClient.onNotification('arandu/status', (status: { state?: string; message?: string }) => {
        if (status.state === 'indexing') {
            setStatus('indexing', status.message);
        } else if (status.state === 'single-file') {
            setStatus('single-file', status.message);
            if (!singleFileNoticeShown) {
                singleFileNoticeShown = true;
                void vscode.window.showInformationMessage(
                    status.message ?? 'No arandu.toml found. Arandu is analyzing files individually.',
                    'Prepare package initialization'
                ).then(action => {
                    if (action === 'Prepare package initialization') {
                        void vscode.commands.executeCommand('arandu.initializePackage');
                    }
                });
            }
        } else if (status.state === 'ready') {
            setStatus(status.message?.includes('stdlib unavailable') ? 'missing'
                : status.message?.startsWith('Single-file') ? 'single-file' : 'ready', status.message);
        } else if (status.state === 'error') {
            setStatus('missing', status.message);
        }
        traceOutputChannel?.info(`Server status: ${status.state ?? 'unknown'}${status.message ? ` — ${status.message}` : ''}`);
    });
    client = nextClient;
    clientStateSubscription = nextClient.onDidChangeState(event => {
        if (event.newState === State.Starting) {
            setStatus('starting');
        } else if (event.newState === State.Running) {
            setStatus('ready');
        } else if (!deactivating) {
            setStatus('stopped', 'Arandu Language Server stopped. Run “Arandu: Restart Language Server”.');
        }
    });

    try {
        await nextClient.start();
        setStatus('ready');
    } catch (error: unknown) {
        if (client === nextClient) {
            client = undefined;
        }
        clientStateSubscription?.dispose();
        clientStateSubscription = undefined;
        const message = error instanceof Error ? error.message : String(error);
        traceOutputChannel?.error(`Failed to start Arandu Language Server: ${message}`);
        setStatus('stopped', `Arandu Language Server failed: ${message}`);
        const action = await vscode.window.showErrorMessage(
            `Failed to start Arandu Language Server: ${message}`,
            'Show Logs'
        );
        if (action === 'Show Logs') {
            traceOutputChannel?.show(true);
        }
    }
}

async function stopClient(): Promise<void> {
    clientStateSubscription?.dispose();
    clientStateSubscription = undefined;
    const activeClient = client;
    client = undefined;
    if (activeClient) {
        try {
            await activeClient.stop(STOP_TIMEOUT_MS);
        } catch (error: unknown) {
            const message = error instanceof Error ? error.message : String(error);
            traceOutputChannel?.warn(`Language server did not stop cleanly: ${message}`);
        }
    }
}

function promptAfterCrashLoop(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): void {
    void handleCrashLoopAction(context, fileWatcher);
}

async function handleCrashLoopAction(
    context: vscode.ExtensionContext,
    fileWatcher: vscode.FileSystemWatcher
): Promise<void> {
    try {
        const action = await vscode.window.showErrorMessage(
            'Arandu Language Server repeatedly crashed. Automatic restart was stopped.',
            'Restart Server',
            'Show Logs'
        );
        if (action === 'Restart Server') {
            await restartLanguageServer(context, fileWatcher);
            return;
        }
        if (action === 'Show Logs') {
            traceOutputChannel?.show(true);
        }
    } catch (error: unknown) {
        const message = error instanceof Error ? error.message : String(error);
        traceOutputChannel?.error(`Failed to handle crash recovery action: ${message}`);
    }
}

async function reportMissingServer(message: string): Promise<void> {
    setStatus('missing', message);
    traceOutputChannel?.error(message);
    const action = await vscode.window.showErrorMessage(message, 'Install SDK', 'Open Settings');
    if (action === 'Install SDK') {
        await vscode.env.openExternal(
            vscode.Uri.parse('https://github.com/arandu-lang/arandu#readme')
        );
    } else if (action === 'Open Settings') {
        await vscode.commands.executeCommand('workbench.action.openSettings', 'arandu.server.path');
    }
}

function setStatus(
    state: ServerUiState,
    detail?: string
): void {
    runtimeState = state;
    if (!statusBarItem) {
        return;
    }
    switch (state) {
        case 'starting':
            statusBarItem.text = '$(sync~spin) Arandu';
            statusBarItem.tooltip = 'Arandu Language Server: Starting';
            break;
        case 'ready':
            statusBarItem.text = '$(check) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Ready';
            statusBarItem.command = 'arandu.showServerLogs';
            break;
        case 'single-file':
            statusBarItem.text = '$(warning) Arandu';
            statusBarItem.tooltip = `${detail ?? 'Single-file analysis'} Click to prepare arandu_cli init.`;
            statusBarItem.command = 'arandu.initializePackage';
            break;
        case 'indexing':
            statusBarItem.text = '$(sync~spin) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Indexing';
            break;
        case 'restarting':
            statusBarItem.text = '$(sync~spin) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Restarting';
            break;
        case 'missing':
            statusBarItem.text = '$(warning) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Executable not found';
            break;
        case 'stopped':
            statusBarItem.text = '$(error) Arandu';
            statusBarItem.tooltip = detail ?? 'Arandu Language Server: Stopped';
            break;
    }
    statusBarItem.show();
}
