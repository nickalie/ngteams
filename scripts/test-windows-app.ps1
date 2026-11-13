# Offline application contracts over synthetic accounts, loopback services and fake CLI tools in the authorized
# Windows VM console. Git for Windows supplies only the fake protocol interpreter behind native .cmd shims.
#Requires -Version 7.0
param(
 [ValidateSet('all','app','ui','scenes')][string]$Group='all',
 [string[]]$Only=@(),
 [switch]$Serve,
 [string]$RunDirectory
)
$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot
Set-Location $root
. "$PSScriptRoot/lib/windows-runner.ps1"
function Run-Child([string]$Executable,[string[]]$Arguments,[string]$Name,[int]$Seconds) {
 $start=[Diagnostics.ProcessStartInfo]::new($Executable)
 $start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.WorkingDirectory=$root
 $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true
 foreach($arg in $Arguments){$start.ArgumentList.Add($arg)}
 $p=[Diagnostics.Process]::Start($start)
 $out=$p.StandardOutput.ReadToEndAsync(); $err=$p.StandardError.ReadToEndAsync()
 try {
  $deadline=[DateTime]::UtcNow.AddSeconds($Seconds)
  while(!$p.WaitForExit(200)){
   if((Get-PSDrive C).Free -lt 1536MB -or [DateTime]::UtcNow -gt $deadline){throw "guard: $Name"}
  }
  $code=$p.ExitCode
 } finally {
  if(!$p.HasExited){$p.Kill($true);$p.WaitForExit(10000)|Out-Null}
  if($out.Wait(10000)){[IO.File]::WriteAllText("$RunDirectory/$Name.out",$out.Result)}
  if($err.Wait(10000)){[IO.File]::WriteAllText("$RunDirectory/$Name.err",$err.Result)}
  $p.Dispose()
 }
 if($code -ne 0){throw "$Name exited $code"}
}
if($Serve){
 $env:NGTEAMS_WINDOWS_VM_SANDBOX='win11'; $env:UI_SANDBOX_DIR=$RunDirectory; $env:GPUI_SMOKE_REQUIRE='1'
 Set-NgteamsMediaRuntime $root "$root/target/windows-media-runtime"
 $env:GST_REGISTRY_1_0="$RunDirectory/registry.bin"
 $exes=Get-Content "$RunDirectory/executables.json" -Raw|ConvertFrom-Json -AsHashtable
 $failures=@()
 foreach($name in $exes.Keys|Sort-Object){
  try{
   $arguments=if($name -like '*-scene-*'){@()}else{@('--test-threads=1','--nocapture')}
   Run-Child $exes[$name] $arguments $name 900
   $result=Get-Content "$RunDirectory/$name.out" -Raw
   if($name -like '*-scene-*'){
    if($result -notmatch '(?m)^shot '){throw "$name did not capture any real scene"}
   }elseif($result -notmatch 'test result: ok\. [1-9][0-9]* passed;'){throw "$name did not report actual test passes"}
  }
  catch{$failures+="$name`: $_"}
 }
 if($failures.Count){$failures|Set-Content "$RunDirectory/failed.txt";exit 1}
 Set-Content "$RunDirectory/passed" 'synthetic application contracts passed'
 exit 0
}
$RunDirectory=Join-Path $root ('target/windows-app-smoke-'+[Guid]::NewGuid().ToString('N'))
$runner=Get-NgteamsRunner $root
& $runner prepare --directory $RunDirectory
if($LASTEXITCODE -ne 0){throw 'Cannot prepare private application test evidence.'}
Set-NgteamsMediaEnvironment $root
$exes=@{}
$targets=@(@('ngteams','lib','ngteams'),@('ngteams','bin','ngteams'),@('teams-ui','lib','teams_ui'))
$scenes=@(
 @('teams-ui','scene','chat_list_window'),
 @('teams-ui','scene','font_styles'),
 @('teams-ui','scene','notes_window'),
 @('teams-ui','scene','meeting_window'),
 @('teams-ui','scene','recap_window'),
 @('teams-ui','scene','workflow_window'),
 @('teams-ui','scene','workflow_message_window'),
 @('teams-ui','scene','workflows_app_window'),
 @('teams-ui','scene','channel_window')
)
# Keep the shared contracts complete as Cargo discovers new integration tests. The Linux compositor harnesses
# and native Windows fixtures have their own drivers; the scene targets below use their own main functions.
$separate=@{
 'ngteams'=@('window','accounts_window','call_window','ui_tour')
 'teams-ui'=@('kit_window','attachment_window','gpui_notification','calendar_window','desktop_bus',
              'native_video','native_desktop','native_shell')+@($scenes|ForEach-Object {$_[2]})
}
foreach($package in @('ngteams','teams-ui')){
 foreach($file in Get-ChildItem -LiteralPath "$root/crates/$package/tests" -Filter '*.rs' -File|Sort-Object Name){
  if($file.BaseName -notin $separate[$package]){$targets+=,@($package,'test',$file.BaseName)}
 }
}
$targets+=$scenes
foreach($target in $targets){
 if(($Group -eq 'app' -and $target[0] -ne 'ngteams') -or ($Group -eq 'ui' -and ($target[0] -ne 'teams-ui' -or $target[1] -eq 'scene')) -or ($Group -eq 'scenes' -and $target[1] -ne 'scene')){continue}
 $package,$kind,$name=$target
 $key="$package-$kind-$name"
 if($Only.Count -and $key -notin $Only){continue}
 $selection=if($kind -eq 'lib'){@('--lib')}elseif($kind -eq 'scene'){@('--test',$name)}else{@("--$kind",$name)}
 $cargoArguments=@('rustc','--profile','test','--locked','-p',$package)+$selection+@('--target-dir','target/lab','-j1','--message-format=json','--','-C','link-arg=/DEBUG:NONE')
 Run-Child (Get-Command cargo -CommandType Application|Select-Object -First 1).Source $cargoArguments "build-$key" 1800
 Get-Content "$RunDirectory/build-$key.out"|ForEach-Object{
  $a=$_|ConvertFrom-Json
  if($a.reason -eq 'compiler-artifact' -and $a.executable -and $a.target.name -eq $name){$exes[$key]=$a.executable}
 }
 if(!$exes.ContainsKey($key)){throw "no test executable for $key"}
 Write-Output "built $key"
}
if(!$exes.Count -or ($Only.Count -and $exes.Count -ne $Only.Count)){throw 'Requested application targets did not all match the selected group.'}
$exes|ConvertTo-Json|Set-Content "$RunDirectory/executables.json"
$taskName='ngteams-app-synthetic-'+[Guid]::NewGuid().ToString('N')
$command='& '+(ConvertTo-NgteamsLiteral $PSCommandPath)+' -Serve -RunDirectory '+(ConvertTo-NgteamsLiteral $RunDirectory)+' *> '+(ConvertTo-NgteamsLiteral "$RunDirectory/console.log")
# The console task starts from the user's profile environment: pass on the test switches set for this run.
foreach($name in @('RECAP_PAGES','NGTEAMS_DEFERRED_STARTUP_REQUIRE_READY')){
 $value=[Environment]::GetEnvironmentVariable($name)
 if($value){$command="`$env:$name="+(ConvertTo-NgteamsLiteral $value)+'; '+$command}
}
$encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
$action=New-ScheduledTaskAction -Execute (Get-Command pwsh -CommandType Application|Select-Object -First 1).Source -Argument "-NoProfile -NonInteractive -EncodedCommand $encoded" -WorkingDirectory $root
$principal=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 55)
try{
 Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings|Out-Null
 Start-ScheduledTask -TaskName $taskName
 $deadline=[DateTime]::UtcNow.AddMinutes(54)
 do{
  Start-Sleep -Seconds 2
  $task=Get-ScheduledTask $taskName
  $info=Get-ScheduledTaskInfo $taskName
  if([DateTime]::UtcNow -gt $deadline){throw 'console suite deadline'}
 }while($task.State -eq 'Running' -or $info.LastTaskResult -eq 267011)
 Write-Output "evidence: $RunDirectory; console result: $($info.LastTaskResult)"
 if(!(Test-Path "$RunDirectory/passed")){if(Test-Path "$RunDirectory/failed.txt"){Get-Content "$RunDirectory/failed.txt"};throw 'synthetic suite failed'}
}finally{
 Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
 Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
}
