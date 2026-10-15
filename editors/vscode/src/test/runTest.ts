import * as path from 'node:path';
import * as fs from 'node:fs';
import { runTests } from '@vscode/test-electron';

async function main(): Promise<void> {
    const extensionDevelopmentPath = path.resolve(__dirname, '..', '..');
    const projectTestsPath = path.resolve(__dirname, 'suite', 'index');
    const projectWorkspacePath = path.resolve(extensionDevelopmentPath, 'src', 'test', 'fixture');
    const standaloneTestsPath = path.resolve(__dirname, 'suite', 'manifestless');
    const standaloneWorkspacePath = path.resolve(extensionDevelopmentPath, 'src', 'test', 'manifestless-fixture');
    const repositoryRoot = path.resolve(extensionDevelopmentPath, '..', '..');
    const serverName = process.platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
    const serverPath = path.join(repositoryRoot, 'target', 'debug', serverName);

    for (const [name, workspacePath, extensionTestsPath] of [
        ['project', projectWorkspacePath, projectTestsPath],
        ['manifestless', standaloneWorkspacePath, standaloneTestsPath]
    ]) {
        const userDataPath = path.join(extensionDevelopmentPath, '.vscode-test', `user-data-${process.pid}-${name}`);
        const extensionsPath = path.join(extensionDevelopmentPath, '.vscode-test', `extensions-${process.pid}-${name}`);
        try {
            await runTests({
                version: '1.92.0',
                extensionDevelopmentPath,
                extensionTestsPath,
                launchArgs: [
                    workspacePath,
                    '--disable-extensions',
                    '--disable-gpu',
                    `--user-data-dir=${userDataPath}`,
                    `--extensions-dir=${extensionsPath}`
                ],
                extensionTestsEnv: {
                    ARANDU_LSP_TEST_PATH: serverPath,
                    ARANDU_LSP_TEST_ALLOW_CRASH: '1'
                }
            });
        } finally {
            fs.rmSync(userDataPath, { recursive: true, force: true });
            fs.rmSync(extensionsPath, { recursive: true, force: true });
        }
    }
}

void main().catch((error: unknown) => {
    console.error(error);
    process.exitCode = 1;
});
