; Reglas del Firewall de Windows para que Home File Sync funcione en redes
; públicas y privadas sin que el usuario tenga que configurar nada.
; (Las redes de eventos suelen clasificarse como "Pública".)

!macro NSIS_HOOK_POSTINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Home File Sync"'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Home File Sync" dir=in action=allow program="$INSTDIR\home-file-sync.exe" enable=yes profile=any'
  nsExec::Exec 'netsh advfirewall firewall add rule name="Home File Sync" dir=in action=allow protocol=UDP localport=5353,47821 program="$INSTDIR\home-file-sync.exe" enable=yes profile=any'
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  nsExec::Exec 'netsh advfirewall firewall delete rule name="Home File Sync"'
!macroend
