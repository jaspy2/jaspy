$projectRoot = Get-Location
$commitHash = (git rev-parse HEAD).Substring(0, 16)
New-Item -ItemType Directory -Force -Path .\publish
Set-Location $projectRoot\src\Jaspy.Switchmaster
dotnet publish -c release -r linux-x64 --self-contained
Compress-Archive -Path .\bin\Release\netcoreapp2.2\linux-x64\publish\** -DestinationPath $projectRoot\publish\jaspy-switchmaster-$commitHash.zip
Set-Location $projectRoot
