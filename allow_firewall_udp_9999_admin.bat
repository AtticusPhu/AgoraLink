@echo off
netsh advfirewall firewall add rule name="AgoraLink UDP 9999" dir=in action=allow protocol=UDP localport=9999
netsh advfirewall firewall add rule name="AgoraLink Discovery UDP 9998" dir=in action=allow protocol=UDP localport=9998
pause
