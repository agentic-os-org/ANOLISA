param(
    [Parameter(Mandatory)][ValidateSet('detect', 'install', 'uninstall')][string]$Action,
    [switch]$DryRun,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'
$interpreter = & $Python -c 'import sys; assert sys.version_info >= (3, 10); print(sys.executable)'
if ($LASTEXITCODE -ne 0 -or !$interpreter) { throw 'Python 3.10 or newer is required; pass -Python with its executable path.' }
$arguments = @('-X', 'utf8', '-B', (Join-Path $PSScriptRoot 'lifecycle.py'), $Action)
if ($DryRun) { $arguments += '--dry-run' }
& $interpreter @arguments
exit $LASTEXITCODE
