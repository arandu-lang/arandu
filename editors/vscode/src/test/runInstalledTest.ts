import * as childProcess from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
    downloadAndUnzipVSCode,
    resolveCliArgsFromVSCodeExecutablePath,
    runTests
} from '@vscode/test-electron';

const VSCODE_VERSION = '1.92.0';

async function main(): Promise<void> {
    const extensionRoot = path.resolve(__dirname, '..', '..');
    const repositoryRoot = path.resolve(extensionRoot, '..', '..');
    const extensionTestsPath = path.resolve(__dirname, 'suite', 'index');
    const harnessPath = path.resolve(extensionRoot, 'src', 'test', 'installedHarness');
    const workspacePath = path.resolve(extensionRoot, 'src', 'test', 'fixture');
    const vsixPath = path.resolve(extensionRoot, '.vscode-test', 'arandu-lang.vsix');
    const serverName = process.platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
    const serverPath = process.env.ARANDU_LSP_TEST_PATH
        ? path.resolve(process.env.ARANDU_LSP_TEST_PATH)
        : path.join(repositoryRoot, 'target', 'debug', serverName);
    const cliName = process.platform === 'win32' ? 'arandu_cli.exe' : 'arandu_cli';
    const aranduCliPath = process.env.ARANDU_CLI_TEST_PATH
        ? path.resolve(process.env.ARANDU_CLI_TEST_PATH)
        : path.join(repositoryRoot, 'target', 'debug', cliName);
    // VS Code's macOS IPC socket includes the user-data path and has a short
    // Unix-domain socket limit. Keep the profile under /tmp with a short name
    // instead of inheriting the long per-user /var/folders path.
    const profileBase = process.platform === 'darwin' ? '/tmp' : os.tmpdir();
    const profileRoot = fs.mkdtempSync(path.join(profileBase, 'av-'));
    const userDataPath = path.join(profileRoot, 'user-data');
    const extensionsPath = path.join(profileRoot, 'extensions');

    if (!fs.existsSync(vsixPath)) {
        throw new Error(`packaged extension missing: ${vsixPath}`);
    }
    if (!fs.existsSync(serverPath)) {
        throw new Error(`Arandu LSP test binary missing: ${serverPath}`);
    }
    if (!fs.existsSync(aranduCliPath)) {
        throw new Error(`Arandu CLI test binary missing: ${aranduCliPath}`);
    }

    // The workspace manifest activates the VSIX as soon as the window opens.
    // Provide the installed SDK paths before startup so activation cannot wait
    // on a missing-server prompt or accidentally select the bundled server.
    const settingsPath = path.join(userDataPath, 'User', 'settings.json');
    fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
    fs.writeFileSync(settingsPath, JSON.stringify({
        'arandu.server.path': serverPath,
        'arandu.cli.path': aranduCliPath,
        'arandu.trace.server': 'verbose'
    }, null, 2));

    const vscodeExecutablePath = await downloadAndUnzipVSCode(VSCODE_VERSION);
    const [cliPath, ...cliArgs] = resolveCliArgsFromVSCodeExecutablePath(vscodeExecutablePath);
    fs.mkdirSync(extensionsPath, { recursive: true });
    const profileArgs = cliArgs.filter(argument =>
        !argument.startsWith('--user-data-dir=') && !argument.startsWith('--extensions-dir=')
    );
    const installArgs = [
        ...profileArgs,
        `--user-data-dir=${userDataPath}`,
        `--extensions-dir=${extensionsPath}`,
        '--install-extension',
        vsixPath,
        '--force'
    ];
    let installExecutable = cliPath;
    let installEnvironment = process.env;
    if (process.platform === 'win32') {
        const vscodeRoot = path.resolve(path.dirname(cliPath), '..');
        installExecutable = path.join(vscodeRoot, 'Code.exe');
        installArgs.unshift(path.join(vscodeRoot, 'resources', 'app', 'out', 'cli.js'));
        installEnvironment = { ...process.env, ELECTRON_RUN_AS_NODE: '1' };
    }
    const install = childProcess.spawnSync(
        installExecutable,
        installArgs,
        { encoding: 'utf8', stdio: 'inherit', env: installEnvironment }
    );
    if (install.status !== 0) {
        throw new Error(`VSIX installation failed with status ${String(install.status)}`);
    }

    try {
        await runTests({
            vscodeExecutablePath,
            extensionDevelopmentPath: harnessPath,
            extensionTestsPath,
            launchArgs: [
                workspacePath,
                `--user-data-dir=${userDataPath}`,
                `--extensions-dir=${extensionsPath}`
            ],
            extensionTestsEnv: {
                ARANDU_LSP_TEST_PATH: serverPath,
                ARANDU_CLI_TEST_PATH: aranduCliPath,
                ARANDU_LSP_TEST_ALLOW_CRASH: '1',
                ARANDU_EXPECT_INSTALLED_EXTENSION: '1',
                ARANDU_INSTALLED_EXTENSIONS_ROOT: extensionsPath
            }
        });
    } catch (error: unknown) {
        // Keep controlled test-profile logs before cleanup so native-only
        // failures retain the LSP/Extension Host evidence needed for diagnosis.
        const logsPath = path.join(userDataPath, 'logs');
        if (fs.existsSync(logsPath)) {
            fs.cpSync(logsPath, path.join(extensionRoot, '.vscode-test', 'failure-logs'), { recursive: true });
        }
        throw error;
    } finally {
        if (process.env.ARANDU_KEEP_VSCODE_TEST_PROFILE === '1') {
            console.error(`Preserved Extension Host profile at ${profileRoot}`);
        } else {
            fs.rmSync(profileRoot, { recursive: true, force: true });
        }
    }
}

void main().catch((error: unknown) => {
    console.error(error);
    process.exitCode = 1;
});
