$timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
$output = @{
    continue = $true
    hookSpecificOutput = @{
        hookEventName = "PreToolUse"
        additionalContext = "Current time: $timestamp"
    }
} | ConvertTo-Json -Compress
Write-Output $output
