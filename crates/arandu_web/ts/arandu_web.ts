/**
 * Arandu In-Browser WebAssembly Compiler and Runner.
 *
 * Provides client-side compilation and execution of Arandu source code
 * directly within the browser (no backend server required).
 */

export interface WebDiagnostic {
  line: number;
  column: number;
  length: number;
  severity: "error" | "warning" | "info";
  code?: string;
  message: string;
  notes: string[];
}

export interface CompileResult {
  success: boolean;
  wasmBytes: Uint8Array | null;
  diagnostics: WebDiagnostic[];
}

export class AranduCompiler {
  private instance: WebAssembly.Instance;
  private memory: WebAssembly.Memory;

  constructor(instance: WebAssembly.Instance) {
    this.instance = instance;
    this.memory = instance.exports.memory as WebAssembly.Memory;
  }

  /**
   * Instantiate compiler from WebAssembly bytes, Response, or URL.
   */
  static async load(source: BufferSource | Response | string): Promise<AranduCompiler> {
    let instance: WebAssembly.Instance;
    if (typeof source === "string") {
      const resp = await fetch(source);
      const res = await WebAssembly.instantiateStreaming(resp, {});
      instance = res.instance;
    } else if (source instanceof Response) {
      const res = await WebAssembly.instantiateStreaming(source, {});
      instance = res.instance;
    } else {
      const res = await WebAssembly.instantiate(source, {});
      instance = res.instance;
    }
    return new AranduCompiler(instance);
  }

  /**
   * Compile Arandu source code into WebAssembly bytecode and diagnostics.
   */
  compile(sourceCode: string): CompileResult {
    const encoder = new TextEncoder();
    const sourceBytes = encoder.encode(sourceCode);
    const sourceLen = sourceBytes.length;

    const alloc = this.instance.exports.arandu_alloc as (size: number) => number;
    const free = this.instance.exports.arandu_free as (ptr: number, size: number) => void;
    const compile = this.instance.exports.arandu_compile as (ptr: number, len: number) => number;
    const freeResponse = this.instance.exports.arandu_free_response as (respPtr: number) => void;

    const sourcePtr = alloc(sourceLen);
    new Uint8Array(this.memory.buffer, sourcePtr, sourceLen).set(sourceBytes);

    const respPtr = compile(sourcePtr, sourceLen);
    if (!respPtr) {
      free(sourcePtr, sourceLen);
      return {
        success: false,
        wasmBytes: null,
        diagnostics: [
          {
            line: 1,
            column: 1,
            length: 1,
            severity: "error",
            code: "ICE001",
            message: "Fatal: internal compiler failure",
            notes: []
          }
        ]
      };
    }

    const view = new DataView(this.memory.buffer, respPtr, 20);
    const success = view.getUint32(0, true) === 1;
    const wasmPtr = view.getUint32(4, true);
    const wasmLen = view.getUint32(8, true);
    const jsonPtr = view.getUint32(12, true);
    const jsonLen = view.getUint32(16, true);

    let wasmBytes: Uint8Array | null = null;
    if (success && wasmPtr !== 0 && wasmLen > 0) {
      wasmBytes = new Uint8Array(this.memory.buffer, wasmPtr, wasmLen).slice();
    }

    let diagnostics: WebDiagnostic[] = [];
    if (jsonPtr !== 0 && jsonLen > 0) {
      const jsonBytes = new Uint8Array(this.memory.buffer, jsonPtr, jsonLen);
      const jsonText = new TextDecoder().decode(jsonBytes);
      try {
        diagnostics = JSON.parse(jsonText);
      } catch {
        diagnostics = [];
      }
    }

    freeResponse(respPtr);
    free(sourcePtr, sourceLen);

    return {
      success,
      wasmBytes,
      diagnostics
    };
  }
}

/**
 * Execute compiled Arandu WebAssembly bytecode in the browser.
 *
 * Connects standard Arandu runtime imports (`env.print_str`, `io.println`) to an output callback.
 */
export async function runWasm(
  wasmBytes: Uint8Array,
  onPrint: (text: string) => void = console.log,
  extraImports: WebAssembly.Imports = {}
): Promise<number> {
  let memory: WebAssembly.Memory;

  const defaultImports: WebAssembly.Imports = {
    env: {
      print_str: (ptr: number, len: number) => {
        const bytes = new Uint8Array(memory.buffer, ptr, len);
        const text = new TextDecoder().decode(bytes);
        onPrint(text);
      },
      ...((extraImports.env as Record<string, Function>) || {})
    },
    io: {
      println: (ptr: number, len: number) => {
        const bytes = new Uint8Array(memory.buffer, ptr, len);
        const text = new TextDecoder().decode(bytes);
        onPrint(text);
      },
      ...((extraImports.io as Record<string, Function>) || {})
    },
    err: {
      new: (ptr: number, _len: number) => {
        return ptr;
      },
      ...((extraImports.err as Record<string, Function>) || {})
    },
    ...extraImports
  };

  const { instance } = await WebAssembly.instantiate(wasmBytes, defaultImports);
  memory = instance.exports.memory as WebAssembly.Memory;

  if (typeof instance.exports.main === "function") {
    const main = instance.exports.main as () => number;
    return main();
  }

  return 0;
}

