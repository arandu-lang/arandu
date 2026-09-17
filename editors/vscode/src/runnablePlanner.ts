export type RunnableAction = 'run' | 'check' | 'test';

export interface FlatRunnableSymbol {
    readonly name: string;
    readonly kind: number;
    readonly line: number;
}

export interface PlannedRunnable {
    readonly line: number;
    readonly action: RunnableAction;
    readonly name: string;
}

// Pure planning for the runnable CodeLens: `main` offers Run and Check, and
// discovered @Test functions offer Run Test. Dedupes so a `main` that is also
// a test yields exactly one lens per action per line. `functionKind` is the
// client-side SymbolKind (e.g. vscode.SymbolKind.Function) because the VS Code
// API re-enumerates LSP kinds, so Function is 11 rather than 12.
export function planRunnables(
    symbols: readonly FlatRunnableSymbol[],
    functionKind: number,
    isTestFunction: (name: string) => boolean
): PlannedRunnable[] {
    const planned: PlannedRunnable[] = [];
    const seen = new Set<string>();
    for (const symbol of symbols) {
        if (symbol.kind !== functionKind) {
            continue;
        }
        if (symbol.name === 'main' && !seen.has(`run:${symbol.line}`)) {
            seen.add(`run:${symbol.line}`);
            planned.push({ line: symbol.line, action: 'run', name: symbol.name });
        }
        if (symbol.name === 'main' && !seen.has(`check:${symbol.line}`)) {
            seen.add(`check:${symbol.line}`);
            planned.push({ line: symbol.line, action: 'check', name: symbol.name });
        }
        if (isTestFunction(symbol.name) && !seen.has(`test:${symbol.line}`)) {
            seen.add(`test:${symbol.line}`);
            planned.push({ line: symbol.line, action: 'test', name: symbol.name });
        }
    }
    return planned;
}