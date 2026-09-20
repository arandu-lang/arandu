# RFC 0016: Sistema de Arquivos Orientado a Capacidades e Resolução Segura de Caminhos contra TOCTOU e Fugas por Symlink

- **Número da RFC:** 0016
- **Título:** Sistema de Arquivos Orientado a Capacidades e Resolução Segura de Caminhos contra TOCTOU e Fugas por Symlink (*Capability-Safe Filesystem & Path Resolution*)
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-19
- **Status:** `Draft`
- **Área Principal:** `Stdlib` / `Runtime` / `Tooling` (`arandu_std`, `arandu_runtime`, `arandu_query`)
- **Documentos Relacionados:**
  - `docs/arandu-stdlib-architecture-v0.1.md`
  - `docs/rfcs/0004-project-package-lifecycle.md`
  - `docs/rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md`
  - `docs/rfcs/0014-native-wasm-component-model-and-runtime.md`

---

## 1. Resumo (Summary)

Esta RFC especifica o modelo arquitetural de interação com o sistema de arquivos para o ecossistema Arandu, eliminando vulnerabilidades de **TOCTOU (*Time-of-Check to Time-of-Use*)**, **corridas de links simbólicos (*Symlink Races*)** e **fuga de limites de diretório (*Path Traversal / Sandbox Escape*)**.

A proposta introduz três pilares integrados:
1. **API de Sistema de Arquivos Orientada a Capacidades (`std.fs.Dir`)**: Transição do modelo tradicional de *autoridade ambiente baseada em strings globais* (`path: str`) para um modelo baseado em *tokens de capacidade de diretório* (`Dir`), onde mutações estruturais e travessias recursivas operam estritamente relativas a descritores abertos pinados pelo kernel.
2. **Motor de Resolução Nativo com Contenção de Kernel**: No runtime (`arandu_runtime`), todas as operações de baixo nível adotam chamadas atômicas baseadas em descritores: `openat2` com `RESOLVE_BENEATH` no Linux (Kernel ≥ 5.6), `openat`/`unlinkat` com `O_NOFOLLOW` no macOS/BSD, e `FILE_FLAG_OPEN_REPARSE_POINT` com semântica POSIX no Windows NT.
3. **Isolamento e Blindagem do VFS do Compilador (`arandu_query::vfs`)**: O mecanismo de descoberta de módulos do compilador (`scan_aru_entries_rec`) e a resolução de pacotes passam a auditar `d_type` e rejeitar a travessia cega de symlinks que apontem para fora da raiz do pacote (`package_src`), garantindo que a compilação incremental no Salsa e no LSP permaneça imune a vetores de fuga do host.

---

## 2. Motivação (Motivation)

### 2.1 O Problema Fundamental: Resolução Textual de Caminhos em Sistemas POSIX

No modelo clássico de I/O de sistemas operacionais Unix-like e Windows, interfaces de alto nível expõem caminhos em strings textuais (`open("/caminho/para/arquivo")`). O kernel resolve essa string passo a passo na árvore de diretórios (*namei* no Linux).

Quando um programa divide uma operação em passos lógicos:
```arandu
// ANTI-PATTERN: Check-then-Act
if fs.isDir("temp/alvo") {
    fs.removeDir("temp/alvo")
}
```
Cria-se uma janela temporal crítica entre o **Check** (`isDir`) e o **Use** (`removeDir`). Em sistemas com concorrência ou processos não confiáveis:
1. O atacante remove `temp/alvo` imediatamente após o `isDir`.
2. O atacante cria um link simbólico `temp/alvo -> /etc` ou `../../host`.
3. O programa executa `removeDir` ou desce recursivamente, apagando ou modificando arquivos fora do escopo pretendido com os privilégios do processo.

### 2.2 Evidências Reais na Indústria e Outras Linguagens

Esta classe de vulnerabilidade é reincidente nos maiores projetos de infraestrutura e linguagens do mundo:

* **CVE-2022-21658 (Rust Standard Library `std::fs::remove_dir_all`)**: O Rust verificava recursivamente se cada entrada era um diretório antes de deletar. Atacantes locais exploravam a corrida substituindo um subdiretório por um symlink, levando à destruição de dados arbitrários no sistema do usuário. A correção forçou a reescrita do motor usando descritores puros (`openat`, `unlinkat`, `O_NOFOLLOW`).
* **Hipervisores e Contêineres (Docker / virtio-fs / runc CVE-2024-21626 / CVE-2026-77179)**: Ao expor montagens de pastas compartilhadas do host para convidados via `virtiofsd`, operações com arquivos abertos e posteriores substituições do diretório pai por symlinks induzem o daemon no host a resolver caminhos fora da raiz compartilhada, resultando em escape total do contêiner para o sistema de arquivos do host.
* **Python (`shutil.rmtree` e PEP 706 / CVE-2007-4559)**: O utilitário padrão de descompactação e deleção sofreu por anos com symlinks maliciosos e travessias `../` (Zip Slip), demandando a introdução de `os.open` com o argumento explícito `dir_fd`.
* **Wasmtime / WASI (CVE-2021-3924)**: O modelo de capacidades do WASI foi violado quando a implementação de resolução de caminhos não normalizados em tempo de execução seguiu links simbólicos relativos criados dentro da sandbox para fora da raiz pré-aberta.

### 2.3 O que acontece se o Arandu não resolver isso na raiz?

Se o Arandu adotar uma biblioteca padrão ingênua baseada apenas em funções globais `fs.removeDirAll(path: str)` ou permitir que o discovery do compilador siga symlinks arbitrariamente:
1. Programas em Arandu estarão vulneráveis a sequestro de I/O em ambientes multiusuário, servidores web, agentes autônomos e sandboxes;
2. Projetos construídos com o compilador Arandu poderão ter seus processos de build comprometidos por pacotes maliciosos com links simbólicos apontando para credenciais do host (`~/.ssh`, chaves de API);
3. O compilador quebrará o determinismo do Salsa e as garantias de early-cutoff se arquivos externos ao pacote forem ingeridos silenciosamente via symlinks voláteis.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 O Conceito: Diretórios como Capacidades (`Dir`)

No Arandu, manipulações estruturais do sistema de arquivos não utilizam autoridade global oculta (*ambient authority*). Para trabalhar dentro de um diretório, o desenvolvedor obtém um handle do tipo `std.fs.Dir`:

```arandu
import std.fs as fs
import std.io as io

@Effects(FileRead, FileWrite)
func processarWorkspace(): Result<void, io.IoError> {
    // 1. Obtém uma capacidade sobre o diretório de trabalho
    let mut dir = fs.openDir("workspace")?
    defer dir.close()

    // 2. Criação e escrita relativa à capacidade
    let mut file = dir.createFile("dados.txt")?
    file.writeAll("Conteúdo Seguro")?
    file.close()

    // 3. Subdiretórios herdam e confinam a capacidade
    let mut sub = dir.openSubdir("cache")?
    defer sub.close()
    
    // 4. Deleção recursiva estritamente confinada (Imune a Symlink Races)
    dir.removeTree("cache")?

    return Result.Ok(())
}
```

### 3.2 O que é Proibido: Tentativa de Escape da Capacidade

Se uma aplicação tentar usar `..` para escapar do diretório associado à capacidade `Dir`, a operação falhará no nível do kernel com o erro `io.IoError.EscapeDenied`:

```arandu
let mut dir = fs.openDir("meu_sandbox")?

// ERRO: O runtime e o kernel barram a travessia acima da raiz da capacidade!
let tentativa = dir.openFile("../arquivo_privado.txt")
// tentativa == Result.Err(io.IoError.EscapeDenied)
```

Mesmo que um processo externo crie concorrentemente um link simbólico dentro de `meu_sandbox/link -> /etc`, operações executadas via `dir` **não seguirão o link simbólico para fora de `meu_sandbox`**.

### 3.3 Tratamento Explícito de Erros de I/O

O Arandu não lança exceções nem entra em pânico em operações de arquivo. Todas as chamadas retornam `Result<T, io.IoError>` com variantes semânticas portáteis:

```arandu
match dir.openFile("config.json") {
    case Result.Ok(file) => {
        // Manipula arquivo aberto
    }
    case Result.Err(io.IoError.NotFound) => {
        // Arquivo inexistente
    }
    case Result.Err(io.IoError.EscapeDenied) => {
        // Tentativa de fuga de diretório bloqueada com segurança
    }
    case Result.Err(io.IoError.SymlinkLoop) => {
        // Ciclo de links detectado
    }
    case Result.Err(e) => {
        // Outros erros de I/O
    }
}
```

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Arquitetura em Camadas da Stdlib (`arandu_core` vs `arandu_std`)

Seguindo estritamente a filosofia de camadas do Arandu definida em `docs/arandu-stdlib-architecture-v0.1.md`:

```text
┌────────────────────────────────────────────────────────┐
│                      arandu_std                        │
│   std.fs.Dir           std.fs.File        std.path     │
│   std.os.descriptors (RawFd, RawHandle, OpenHow)       │
└──────────────────────────┬─────────────────────────────┘
                           ▼
┌────────────────────────────────────────────────────────┐
│                      arandu_alloc                      │
│   String, Vec, PathBuf, Alocadores de Descritores      │
└──────────────────────────┬─────────────────────────────┘
                           ▼
┌────────────────────────────────────────────────────────┐
│                      arandu_core                       │
│   Result<T, E>, Option<T>, RawPtr (Zero OS, Zero I/O)  │
└────────────────────────────────────────────────────────┘
```

1. **`arandu_core`**: Permanece 100% puro. Não contém tipos de arquivo, não conhece descritores de sistema operacional e não assume a presença de threads ou I/O.
2. **`arandu_std::os::descriptors`**: Introduz os wrappers de baixo nível de zero custo para identificadores opacos do host:
   ```arandu
   public struct DirDescriptor {
       fd: int // POSIX fd ou Windows HANDLE compactado
   }
   ```
3. **`arandu_std::fs::Dir`**: Expõe métodos de alto nível com tipagem forte e checagem de erros.

### 4.2 Motor de Execução Nativo no Runtime (`arandu_runtime::fs_runtime`)

O runtime em Rust que serve de base para o backend Cranelift (JIT/AOT) e os emissores C implementa contenção nativa adaptada por plataforma:

#### 4.2.1 Linux (Kernel ≥ 5.6): `openat2` com `RESOLVE_BENEATH`

No Linux, todas as aberturas relativas a um `Dir` utilizam a syscall `openat2`:

```c
struct open_how how;
memset(&how, 0, sizeof(how));
how.flags = flags | O_CLOEXEC;
how.mode = mode;
how.resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS;

int fd = syscall(SYS_openat2, dir_fd, rel_path, &how, sizeof(how));
```

* **`RESOLVE_BENEATH`**: O kernel do Linux rejeita atomicamente qualquer resolução de caminho que ultrapasse o diretório `dir_fd` (inclusive via symlinks absolutos como `/etc` ou sequências de `../../`).
* **`RESOLVE_NO_MAGICLINKS`**: Bloqueia links especiais do `/proc/[pid]/fd/*`, impedindo ataques modernos de substituição de descritores de contêiner.

Em kernels Linux legados (< 5.6), o runtime executa fallback baseado em descritores sequenciais com `openat(..., O_NOFOLLOW | O_CLOEXEC)` e validação via `fstat` comparando os dispositivos e inodes (`st_dev`, `st_ino`).

#### 4.2.2 macOS / FreeBSD / Darwin

Em sistemas BSD e macOS onde `openat2` não está presente:
1. Travessias recursivas (`removeTree`) utilizam a pilha de descritores abertos com `openat(parent_fd, name, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`.
2. As deleções de nós utilizam `unlinkat(parent_fd, name, AT_REMOVEDIR)` e `unlinkat(parent_fd, name, 0)`.
3. Em nenhum momento o caminho textual completo é re-resolvido após o início da operação. No Darwin, onde disponível, utiliza-se a flag de controle `fcntl(fd, F_RESOLVE_BENEATH)`.

#### 4.2.3 Windows NT

No Windows, o modelo de segurança baseia-se em handles de arquivo com proteção estrita contra pontos de junção (*NTFS Junctions / Reparse Points*):
1. Abertura do diretório base via `CreateFileW` com `FILE_FLAG_BACKUP_SEMANTICS`.
2. Abertura de subitens com a flag **`FILE_FLAG_OPEN_REPARSE_POINT`**: garante que links simbólicos e junções do Windows sejam abertos como o próprio link, sem seguir o destino no filesystem.
3. Operações de deleção atômica via `SetFileInformationByHandle` utilizando `FILE_DISPOSITION_INFO_EX` com as flags:
   - `FILE_DISPOSITION_FLAG_DELETE`: Marca para deleção.
   - `FILE_DISPOSITION_FLAG_POSIX_SEMANTICS`: Permite a deleção de arquivos abertos e remoção atômica sem travas de compartilhamento legado do Win32.
   - `FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE`: Evita falhas silenciosas de deleção em arquivos marcados como somente leitura.

#### 4.2.4 WebAssembly / WASI (`arandu_backend_wasm` / RFC 0014)

O struct `std.fs.Dir` mapeia **1:1** para o modelo do WASI (*WebAssembly System Interface*), especificamente `wasi:filesystem/types.descriptor`. Como o WASI é nativamente baseado em capacidades com diretórios pré-abertos, programas Arandu compilam para WebAssembly sem overhead de tradução de caminhos e com isolamento garantido pela máquina virtual WASM.

### 4.3 Algoritmo de Deleção e Varredura Segura (`removeTree`)

Para evitar a vulnerabilidade do Rust (CVE-2022-21658), o algoritmo de deleção recursiva de diretórios em `arandu_runtime` nunca deve utilizar recursão ingênua por strings. O algoritmo canônico é implementado por descritores:

```text
Algoritmo: SafeRemoveTree(dir_fd)
1. Abrir stream de diretório fdopendir(dir_fd)
2. Para cada entrada no diretório (readdir):
     a. Ignorar "." e ".."
     b. Inspecionar d_type:
        - Se d_type == DT_UNKNOWN: Chamar fstatat(dir_fd, name, AT_SYMLINK_NOFOLLOW)
        - Se for LINK SIMBÓLICO: Chamar unlinkat(dir_fd, name, 0)  // Deleta o link, NÃO o alvo!
        - Se for ARQUIVO NORMAL/FIFO/SOCKET: Chamar unlinkat(dir_fd, name, 0)
        - Se for DIRETÓRIO:
            i.   child_fd = openat(dir_fd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            ii.  Se falhar com ELOOP / ENOTDIR: É um symlink que se disfarçou! Tratar como link e deletar.
            iii. SafeRemoveTree(child_fd)
            iv.  close(child_fd)
            v.   unlinkat(dir_fd, name, AT_REMOVEDIR)
3. Fechar stream de diretório.
```

### 4.4 Blindagem do VFS e Resolução de Módulos no Compilador (`arandu_query`)

O compilador Arandu precisa de proteção equivalente durante a fase de descoberta de arquivos de pacotes (`arandu_query::vfs`).

#### 4.4.1 Correção em `scan_aru_entries_rec`
Em `crates/arandu_query/src/vfs.rs`, a implementação atual:
```rust
// Código atual vulnerável a symlink loops e fugas:
for entry in rd.flatten() {
    let path = entry.path();
    if path.is_dir() { // <-- segue symlinks via stat!
        scan_aru_entries_rec(root, &path, out);
    }
}
```
Será substituída por:
```rust
// Implementação segura:
for entry in rd.flatten() {
    let Ok(ft) = entry.file_type() else { continue };
    if ft.is_symlink() {
        // Symlinks não são seguidos cegamente durante discovery de pacotes.
        // Se o destino estiver fora de 'root', emitir diagnóstico de integridade.
        continue;
    }
    let path = entry.path();
    if ft.is_dir() {
        scan_aru_entries_rec(root, &path, out);
    } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("aru") {
        if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}
```

#### 4.4.2 Confinamento em `map_import_key`
O resolvedor de chaves de importação em Salsa continua validando que nenhuma chave contenha `..` e que todo caminho físico resolvido comece estritamente com `roots.package_src` ou `roots.stdlib_root`.

### 4.5 Integração com o Sistema de Efeitos (`@Effects`)

O Arandu distingue operações com o sistema de arquivos entre **autoridade ambiente** e **autoridade de capacidade**:

| Efeito | Descrição | Escopo Permitido |
| :--- | :--- | :--- |
| `@Effects(FileRead)` | Leitura via descritor ou handle controlado (`Dir`, `File`) | Aplicações, libs com permissão local |
| `@Effects(FileWrite)` | Escrita ou mutação via descritor controlado | Aplicações, libs com permissão local |
| `@Effects(AmbientFsRead)` | Abertura arbitrária de caminhos globais do host | Restrito (raiz da aplicação) |
| `@Effects(AmbientFsWrite)` | Criação arbitrária de caminhos globais do host | Altamente restrito / auditável |

No arquivo `arandu.toml` (política de pacotes, RFC 0004), projetos dependentes podem ser configurados para negar autoridade ambiente:
```toml
[capabilities]
deny = ["AmbientFsRead", "AmbientFsWrite"]
```
Quando negado, uma biblioteca dependente **só pode acessar os diretórios que o binário principal passar explicitamente como argumento `Dir`**, garantindo confinamento em tempo de compilação.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Preservação dos Invariantes Centrais do Arandu

1. **Queries Salsa Puras e Determinísticas**: Nenhuma chamada ao sistema de arquivos (nem mesmo via descritores) pode acontecer dentro de queries tracked (`resolve`, `type_check`, `lower_amir`). O I/O continua restrito aos inputs Salsa (`DirectoryListing` e `SourceFile`).
2. **Sem Alocações Ocultas (*No hidden allocation*)**: A estrutura `Dir` encapsula apenas um número inteiro escalar (`fd: i32` ou `handle: usize`) alocado na pilha.
3. **Sem Runtime Implícito (*No implicit runtime*)**: Operações com `Dir` são síncronas e diretas ao kernel por padrão. Se usadas em contexto assíncrono com `arandu_std::io`, integram-se ao `EpollReactor` (SL_R) sem criar threads implícitas.
4. **Layout Dependente do Alvo**: `DirDescriptor` respeita o tamanho de ponteiro/word do alvo através de `TargetInfo`.

### 5.2 Desvantagens e Custos de Complexidade

* **Custo em Sistemas Legados**: Em kernels Linux anteriores a 5.6 ou sistemas sem `openat2`, o fallback exige manter um descritor de arquivo aberto para cada nível do diretório durante travessias profundas, o que consome file descriptors temporários do processo.
* **Ergonomia Cognitiva**: Desenvolvedores acostumados com APIs ingênuas de script (`fs.readFile("/caminho/completo")`) precisarão aprender o padrão de obter um `Dir` raiz (`fs.openDir`) ou usar explicitamente a função restrita com anotação de efeito ambiente.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Alternativa 1: Canonicalização Prévia de Strings (`realpath` / `canonicalize`)
* **Abordagem:** Antes de qualquer operação de arquivo, converter o caminho em um caminho absoluto resolvido e verificar se ele começa com o prefixo permitido (`path.startsWith("/sandbox")`).
* **Por que foi rejeitada:** É a definição literal de uma falha TOCTOU. Entre o retorno de `realpath` e a chamada subsequente de `open`, o diretório pai ou qualquer nó intermediário pode ser substituído por um symlink, tornando a verificação inútil.

### Alternativa 2: Confiar no `chroot` ou Linux Namespaces
* **Abordagem:** Isolar processos inteiros usando `chroot` ou user namespaces (`unshare`).
* **Por que foi rejeitada:** Inviável para uma biblioteca padrão de uso geral. Exige privilégios de superusuário (`root`/`CAP_SYS_ADMIN`), não funciona de maneira uniforme entre threads de um mesmo processo e não possui suporte portável em Windows ou WebAssembly.

### Alternativa 3: Adotar a Abordagem Baseada em Capacidades (Abordagem Escolhida)
* **Por que foi escolhida:** Move a responsabilidade de atomicidade para o kernel através de descritores de arquivos já abertos. Funciona de maneira uniforme no Linux (`openat2`), BSD/macOS (`openat`/`unlinkat`), Windows (`FILE_FLAG_OPEN_REPARSE_POINT`) e WebAssembly (WASI), oferecendo a máxima segurança possível na arquitetura atual de computadores sem penalidades de desempenho.

---

## 7. Arte Prévia e Literatura Científica (Prior Art)

### 7.1 Literatura Acadêmica

1. **Watson, R. N., Anderson, J., Laurie, B., & Kennaway, K. (2010).** *"Capsicum: practical capabilities for UNIX."* In *19th USENIX Security Symposium (USENIX Security 10)*.
   - Demonstra como decompor aplicações em torno de capacidades de descritores de arquivos elimina completamente classes inteiras de ataques confused-deputy e travessias não autorizadas.
2. **Tsafrir, D., Hertz, T., Wagner, D. A., & Da Silva, D. (2008).** *"Portably Solving File TOCTTOU Races with Hardness Amplification."* In *6th USENIX Conference on File and Storage Technologies (FAST 08)*.
   - Analisa formalmente as vulnerabilidades de corrida em sistemas de arquivos e comprova a ineficácia de verificações em espaço de usuário frente a ataques concorrentes.
3. **Wei, J., & Pu, C. (2005).** *"TOCTTOU Vulnerabilities in UNIX-Style File Systems: An Anatomical Study."* In *4th Annual FAST Conference*.
   - Taxonomia abrangente sobre como a separação entre resolução de nomes e operações de inodes gera brechas exploráveis.

### 7.2 Implementações em Outras Linguagens

* **Zig (`std.fs.Dir`)**: O ecossistema Zig adota `Dir` como objeto primário de I/O. Funções de biblioteca recebem um `std.fs.Dir` e realizam operações relativas. O Arandu adota essa elegância de design combinando-a com seu sistema tipado de efeitos (`@Effects`).
* **Rust (`std::fs` e crate `cap-std`)**: O Rust corrigiu a CVE-2022-21658 em seu runtime nativo, e a comunidade desenvolveu o projeto `cap-std` para fornecer segurança baseada em capacidades. A stdlib do Arandu incorpora essas proteções diretamente no núcleo de `std.fs`, sem necessidade de bibliotecas externas.
* **Go (`os.DirFS` e `io/fs`)**: O Go introduziu interfaces de sistemas de arquivos virtuais somente de leitura (`io/fs`), mas manteve APIs de mutação como `os.RemoveAll` baseadas em strings globais, mantendo riscos de corrida históricos em Windows e UNIX.
* **Kernel Linux (`openat2`)**: O trabalho de Aleksa Sarai (mantenedor do `runc`) introduziu flags formais de resolução no Linux 5.6 especificamente para conter fugas de contêineres e sandboxes, servindo de fundação técnica para o runtime do Arandu.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Granularidade dos Efeitos em `Dir`**: Avaliar se devemos separar `@Effects(FileRead)` de `@Effects(FileWrite)` nos métodos individuais de `Dir` ou se métodos de criação/deleção exigirão uma anotação combinada `@Effects(DirMutate)`.
2. **Suporte a Symlinks Legítimos Dentro da Capacidade**: Permitir uma opção em `OpenDirOptions` para seguir links simbólicos relativos contanto que o kernel garanta que o alvo resida comprovadamente sob a mesma raiz (`RESOLVE_BENEATH` já faz isso nativamente no Linux, mas exige validação cuidadosa no fallback de macOS e Windows).

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Sandboxing de CTFE e Comptime (RFC 0013)**: Quando macros e metaprogramação em tempo de compilação forem introduzidas no Arandu, todo acesso a arquivos durante a compilação será estritamente mediado por instâncias de `Dir` virtuais gerenciadas pelo VFS do Salsa, tornando impossível que uma dependência maliciosa roube dados do host durante `arandu build`.
2. **FS Transacional e Copy-on-Write**: Extensão da abstração de `Dir` para suportar transações temporárias isoladas e snapshots em memória para o harness de testes (`std.testing`).
3. **Controle Fino de Direitos (Estilo Capsicum/FreeBSD)**: Capacidade de limitar os direitos de um `DirDescriptor` em tempo de execução através de flags de restrição irreversíveis (`dir.limitRights(ReadOnly)`).
