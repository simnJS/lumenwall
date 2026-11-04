<!-- downloads -->
## Download

**Windows 10 and 11:** download **[Lumenwall-Setup.exe](https://github.com/simnJS/lumenwall/releases/download/v@VERSION@/Lumenwall-Setup.exe)** and run it. The installer is not code-signed yet, so SmartScreen may stop it: click **More info › Run anyway**.

**Linux** (x86-64, for your account, no sudo):

```sh
curl -fsSL https://github.com/simnJS/lumenwall/releases/download/v@VERSION@/install.sh | sh -s -- --version @VERSION@
```

**Debian, Ubuntu** (system-wide): download [lumenwall-@VERSION@-amd64.deb](https://github.com/simnJS/lumenwall/releases/download/v@VERSION@/lumenwall-@VERSION@-amd64.deb), then `sudo apt install ./lumenwall-@VERSION@-amd64.deb`.

The `.zip` (Windows) and `.tar.gz` (Linux) hold the same programs without an installer; Lumenwall's built-in updater uses them. `SHA256SUMS` lists the SHA-256 of every file and `SHA256SUMS.minisig` signs it ([verify a download](https://github.com/simnJS/lumenwall/blob/main/docs/RELEASING.md#verify-a-download)).
<!-- /downloads -->

