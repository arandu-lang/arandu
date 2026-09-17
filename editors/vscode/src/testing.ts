import * as childProcess from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';
import {
    BENCH_LIST_PROTOCOL_V1,
    TEST_LIST_PROTOCOL_V1,
    TestJsonCase,
    parseDiscoveryReport,
    parseTestReport
} from './testingProtocol';

interface DiscoveredCase {
    readonly canonicalId: string;
    readonly root: string;
}

export interface TestingIntegration extends vscode.Disposable {
    getDiscoveredCount(): number;
    runFirstForTest(): Promise<string>;
    /** Canonical CLI id of the discovered test whose last path segment is the
     * given function name, or undefined when the function is not a test. */
    getTestIdForFunction(uri: vscode.Uri, functionName: string): string | undefined;
    /** Run the discovered test for a function and return its status string. */
    runTestById(uri: vscode.Uri, functionName: string): Promise<string>;
    /** Fires whenever the discovered test tree changes. */
    readonly onDidChangeTests: vscode.Event<void>;
}

export function createTestingIntegration(
    context: vscode.ExtensionContext,
    watcher: vscode.FileSystemWatcher,
    output: vscode.LogOutputChannel
): TestingIntegration {
    const controller = vscode.tests.createTestController('aranduTests', 'Arandu Tests');
    const cases = new Map<string, DiscoveredCase>();
    const disposables: vscode.Disposable[] = [controller];
    const testChanges = new vscode.EventEmitter<void>();
    disposables.push(testChanges);
    let refreshGeneration = 0;

    const refresh = async (): Promise<void> => {
        const generation = ++refreshGeneration;
        const nextCases = new Map<string, DiscoveredCase>();
        const roots: vscode.TestItem[] = [];
        for (const folder of vscode.workspace.workspaceFolders ?? []) {
            if (folder.uri.scheme !== 'file') {
                continue;
            }
            const cli = discoverCli(context, folder.uri.fsPath);
            if (!cli) {
                output.warn(`Testing unavailable for ${folder.name}: could not find arandu CLI`);
                continue;
            }
            const listed = await runCli(cli, ['test', folder.uri.fsPath, '--list', '--format', 'json']);
            if (listed.code !== 0) {
                output.warn(`Test discovery failed for ${folder.name}: ${listed.stderr.trim()}`);
                continue;
            }
            const discovery = parseDiscoveryReport(listed.stdout, TEST_LIST_PROTOCOL_V1);
            if (!discovery) {
                output.warn(`Test discovery returned an invalid report for ${folder.name}`);
                continue;
            }
            const rootId = `workspace:${folder.uri.toString(true)}`;
            const root = controller.createTestItem(rootId, folder.name, folder.uri);
            for (const discovered of discovery.cases) {
                const internalId = `${rootId}:${discovered.id}`;
                const uri = vscode.Uri.joinPath(folder.uri, ...discovered.path.split('/'));
                const item = controller.createTestItem(internalId, discovered.id, uri);
                item.range = new vscode.Range(
                    discovered.line,
                    discovered.column_utf16,
                    discovered.line,
                    discovered.column_utf16
                );
                root.children.add(item);
                nextCases.set(internalId, { canonicalId: discovered.id, root: folder.uri.fsPath });
            }
            roots.push(root);
        }
        if (generation !== refreshGeneration) {
            return;
        }
        cases.clear();
        for (const [id, testCase] of nextCases) {
            cases.set(id, testCase);
        }
        controller.items.replace(roots);
        testChanges.fire();
    };

    controller.refreshHandler = refresh;
    const runProfile = controller.createRunProfile(
        'Run',
        vscode.TestRunProfileKind.Run,
        (request, token) => runTests(controller, cases, request, token, context, output),
        true
    );
    disposables.push(runProfile);

    let refreshTimer: NodeJS.Timeout | undefined;
    const scheduleRefresh = (): void => {
        if (refreshTimer) {
            clearTimeout(refreshTimer);
        }
        refreshTimer = setTimeout(() => {
            void refresh().catch((error: unknown) => {
                output.error(`Test discovery failed: ${errorMessage(error)}`);
            });
        }, 250);
    };
    disposables.push(
        watcher.onDidCreate(scheduleRefresh),
        watcher.onDidDelete(scheduleRefresh),
        watcher.onDidChange(scheduleRefresh),
        vscode.commands.registerCommand('arandu.refreshTests', async () => refresh()),
        vscode.commands.registerCommand('arandu.runBenchmark', async () => runBenchmark(context, output)),
        new vscode.Disposable(() => {
            if (refreshTimer) {
                clearTimeout(refreshTimer);
            }
        })
    );
    void refresh().catch((error: unknown) => output.error(`Test discovery failed: ${errorMessage(error)}`));
    const disposable = vscode.Disposable.from(...disposables);
    return {
        dispose: (): void => {
            disposable.dispose();
        },
        getDiscoveredCount: (): number => cases.size,
        runFirstForTest: async (): Promise<string> => {
            const discovered = cases.values().next().value;
            if (!discovered) {
                throw new Error('No discovered Arandu test is available');
            }
            const cli = discoverCli(context, discovered.root);
            if (!cli) {
                throw new Error('Could not find the Arandu CLI');
            }
            const result = await runCli(cli, [
                'test', discovered.root, '--exact', discovered.canonicalId, '--format', 'json'
            ]);
            const report = parseTestReport(result.stdout);
            const testCase = report?.cases.find(entry => entry.id === discovered.canonicalId);
            if (!testCase) {
                throw new Error(result.stderr.trim() || 'Invalid Arandu test report');
            }
            return testCase.status;
        },
        getTestIdForFunction: (uri, functionName): string | undefined => {
            const found = findTestForFunction(controller, cases, uri, functionName);
            return found?.canonicalId;
        },
        runTestById: async (uri, functionName): Promise<string> => {
            const found = findTestForFunction(controller, cases, uri, functionName);
            if (!found) {
                throw new Error(`No Arandu test discovered for function “${functionName}”`);
            }
            const statuses = new Map<string, string>();
            const source = new vscode.CancellationTokenSource();
            try {
                await runTests(
                    controller,
                    cases,
                    new vscode.TestRunRequest([found.item]),
                    source.token,
                    context,
                    output,
                    statuses
                );
            } finally {
                source.dispose();
            }
            return statuses.get(found.item.id) ?? 'failed';
        },
        onDidChangeTests: testChanges.event
    };
}

interface FoundTest {
    readonly item: vscode.TestItem;
    readonly canonicalId: string;
}

function findTestForFunction(
    controller: vscode.TestController,
    cases: ReadonlyMap<string, DiscoveredCase>,
    uri: vscode.Uri,
    functionName: string
): FoundTest | undefined {
    let found: FoundTest | undefined;
    const visit = (item: vscode.TestItem): void => {
        const entry = cases.get(item.id);
        if (found === undefined && entry && item.uri?.toString() === uri.toString()) {
            const separator = entry.canonicalId.lastIndexOf('::');
            const last = separator >= 0 ? entry.canonicalId.slice(separator + 2) : entry.canonicalId;
            if (last === functionName) {
                found = { item, canonicalId: entry.canonicalId };
                return;
            }
        }
        item.children.forEach(visit);
    };
    controller.items.forEach(visit);
    return found;
}

async function runTests(
    controller: vscode.TestController,
    cases: ReadonlyMap<string, DiscoveredCase>,
    request: vscode.TestRunRequest,
    token: vscode.CancellationToken,
    context: vscode.ExtensionContext,
    output: vscode.LogOutputChannel,
    statuses?: Map<string, string>
): Promise<void> {
    const run = controller.createTestRun(request);
    const selected = collectSelected(controller, cases, request);
    try {
        for (const item of selected) {
            if (token.isCancellationRequested) {
                run.skipped(item);
                continue;
            }
            const discovered = cases.get(item.id);
            if (!discovered) {
                continue;
            }
            const cli = discoverCli(context, discovered.root);
            if (!cli) {
                run.errored(item, new vscode.TestMessage('Could not find the Arandu CLI'));
                continue;
            }
            run.enqueued(item);
            run.started(item);
            const result = await runCli(
                cli,
                ['test', discovered.root, '--exact', discovered.canonicalId, '--format', 'json'],
                token
            );
            const report = parseTestReport(result.stdout);
            const testCase = report?.cases.find(entry => entry.id === discovered.canonicalId);
            statuses?.set(item.id, testCase?.status ?? 'failed');
            appendOutput(run, testCase?.stdout ?? result.stdout, item);
            appendOutput(run, testCase?.stderr ?? result.stderr, item);
            if (!testCase) {
                run.errored(item, new vscode.TestMessage(result.stderr.trim() || 'Invalid Arandu test report'));
                continue;
            }
            const duration = Number.isFinite(testCase.duration_ms) ? testCase.duration_ms : undefined;
            switch (testCase.status) {
                case 'passed':
                    run.passed(item, duration);
                    break;
                case 'skipped':
                    run.skipped(item);
                    break;
                case 'failed':
                    run.failed(item, testMessage(testCase), duration);
                    break;
                case 'timed_out':
                case 'crashed':
                    run.errored(item, testMessage(testCase), duration);
                    break;
            }
        }
    } catch (error: unknown) {
        output.error(`Test execution failed: ${errorMessage(error)}`);
    } finally {
        run.end();
    }
}

function collectSelected(
    controller: vscode.TestController,
    cases: ReadonlyMap<string, DiscoveredCase>,
    request: vscode.TestRunRequest
): vscode.TestItem[] {
    const excluded = new Set(request.exclude?.map(item => item.id) ?? []);
    const result: vscode.TestItem[] = [];
    const visit = (item: vscode.TestItem): void => {
        if (excluded.has(item.id)) {
            return;
        }
        if (cases.has(item.id)) {
            result.push(item);
        }
        item.children.forEach(visit);
    };
    if (request.include) {
        for (const item of request.include) {
            visit(item);
        }
    } else {
        controller.items.forEach(visit);
    }
    return result;
}

async function runBenchmark(
    context: vscode.ExtensionContext,
    output: vscode.LogOutputChannel
): Promise<void> {
    const folder = vscode.workspace.workspaceFolders?.find(candidate => candidate.uri.scheme === 'file');
    if (!folder) {
        await vscode.window.showErrorMessage('Open an Arandu workspace before running a benchmark.');
        return;
    }
    const cli = discoverCli(context, folder.uri.fsPath);
    if (!cli) {
        await vscode.window.showErrorMessage('Could not find the Arandu CLI. Configure arandu.cli.path.');
        return;
    }
    const listed = await runCli(cli, ['bench', folder.uri.fsPath, '--list', '--format', 'json']);
    if (listed.code !== 0) {
        output.error(listed.stderr);
        output.show(true);
        return;
    }
    const discovery = parseDiscoveryReport(listed.stdout, BENCH_LIST_PROTOCOL_V1);
    if (!discovery) {
        output.error('Benchmark discovery returned an invalid report.');
        output.show(true);
        return;
    }
    const ids = discovery.cases.map(testCase => testCase.id);
    const selected = await vscode.window.showQuickPick(ids, { placeHolder: 'Select an Arandu benchmark' });
    if (!selected) {
        return;
    }
    output.show(true);
    output.info(`Running benchmark ${selected}`);
    const result = await runCli(cli, ['bench', folder.uri.fsPath, '--exact', selected, '--format', 'json']);
    if (result.stdout.trim()) {
        output.info(result.stdout.trim());
    }
    if (result.stderr.trim()) {
        output.error(result.stderr.trim());
    }
}

export function discoverCli(context: vscode.ExtensionContext, workspaceRoot: string): string | undefined {
    const configured = vscode.workspace.getConfiguration('arandu').get<string | null>('cli.path')?.trim();
    if (configured && path.isAbsolute(configured) && executable(configured)) {
        return configured;
    }
    const suffix = process.platform === 'win32' ? '.exe' : '';
    const server = vscode.workspace.getConfiguration('arandu').get<string | null>('server.path')?.trim();
    const candidates = [
        server ? path.join(path.dirname(server), `arandu${suffix}`) : undefined,
        server ? path.join(path.dirname(server), `arandu_cli${suffix}`) : undefined,
        path.join(workspaceRoot, 'target', 'release', `arandu_cli${suffix}`),
        path.join(workspaceRoot, 'target', 'debug', `arandu_cli${suffix}`),
        path.resolve(context.extensionPath, '..', '..', 'target', 'release', `arandu_cli${suffix}`),
        path.resolve(context.extensionPath, '..', '..', 'target', 'debug', `arandu_cli${suffix}`)
    ];
    for (const candidate of candidates) {
        if (candidate && executable(candidate)) {
            return candidate;
        }
    }
    return findOnPath(`arandu${suffix}`) ?? findOnPath(`arandu_cli${suffix}`);
}

function executable(candidate: string): boolean {
    try {
        fs.accessSync(candidate, process.platform === 'win32' ? fs.constants.F_OK : fs.constants.X_OK);
        return fs.statSync(candidate).isFile();
    } catch {
        return false;
    }
}

function findOnPath(name: string): string | undefined {
    for (const directory of (process.env.PATH ?? '').split(path.delimiter).filter(Boolean)) {
        const candidate = path.join(directory, name);
        if (executable(candidate)) {
            return candidate;
        }
    }
    return undefined;
}

function runCli(
    executablePath: string,
    args: readonly string[],
    cancellation?: vscode.CancellationToken
): Promise<{ readonly code: number; readonly stdout: string; readonly stderr: string }> {
    return new Promise(resolve => {
        const child = childProcess.spawn(executablePath, args, {
            windowsHide: true,
            detached: process.platform !== 'win32',
            stdio: ['ignore', 'pipe', 'pipe']
        });
        const stdout: Buffer[] = [];
        const stderr: Buffer[] = [];
        let completed = false;
        const finish = (result: { readonly code: number; readonly stdout: string; readonly stderr: string }): void => {
            if (completed) {
                return;
            }
            completed = true;
            cancellationSubscription?.dispose();
            resolve(result);
        };
        child.stdout.on('data', (chunk: Buffer) => stdout.push(chunk));
        child.stderr.on('data', (chunk: Buffer) => stderr.push(chunk));
        const cancellationSubscription = cancellation?.onCancellationRequested(() => {
            if (process.platform === 'win32' && child.pid !== undefined) {
                const killer = childProcess.spawn('taskkill', ['/F', '/T', '/PID', String(child.pid)], {
                    windowsHide: true,
                    stdio: 'ignore'
                });
                killer.unref();
            } else if (child.pid !== undefined) {
                try {
                    process.kill(-child.pid, 'SIGKILL');
                } catch {
                    child.kill();
                }
            }
        });
        child.on('error', error => {
            finish({ code: 1, stdout: '', stderr: error.message });
        });
        child.on('close', code => {
            finish({
                code: code ?? 1,
                stdout: Buffer.concat(stdout).toString('utf8'),
                stderr: Buffer.concat(stderr).toString('utf8')
            });
        });
    });
}

function testMessage(testCase: TestJsonCase): vscode.TestMessage {
    const message = new vscode.TestMessage(testCase.failure?.message ?? testCase.status);
    return message;
}

function appendOutput(run: vscode.TestRun, value: string, item: vscode.TestItem): void {
    if (value) {
        run.appendOutput(value.replace(/\r?\n/gu, '\r\n'), undefined, item);
    }
}

function errorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}
