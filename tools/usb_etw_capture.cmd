@echo off
rem USB ETW trace of the PIX-DT295 vendor driver (run as Administrator).
rem
rem USBPcap cannot see this driver: pxmb86m0x_usb.sys sends its URBs straight to
rem the PDO (KSDEVICE->PhysicalDeviceObject), bypassing the USBPcap filter that
rem sits on top of the PDO. The USB3 stack's ETW providers (UCX / USBXHCI) log
rem below the hub, so they see every URB: setup packets (bRequest/wValue/wIndex/
rem wLength) and bulk lengths. Payload bytes are NOT recorded: the data-bearing
rem UCX events (22/29) never appear even with FullDataBusTrace set, so register
rem values and doorbell bytes cannot be recovered from the trace.
rem
rem Usage:  usb_etw_capture.cmd [SERIAL]
rem         SERIAL = the unit's USB serial number (dt295 list). With it the unit is
rem         restarted once the trace runs, so the driver's start-up is captured too.
rem Output: temp\usbtrace.etl
rem Decode: tracerpt temp\usbtrace.etl -o usbtrace.xml -of XML -y
rem         dt295 etw-urbs usbtrace.xml log.tsv
setlocal
set OUT=%~dp0..\temp\usbtrace.etl
set SERIAL=%~1

logman stop usbtrace -ets >nul 2>&1
if exist "%OUT%" del "%OUT%"

rem keywords: Default 0x1 | HeadersBusTrace 0x40 | FullDataBusTrace 0x100
logman create trace usbtrace -ets -o "%OUT%" -nb 128 1024 -bs 1024 -mode Sequential -p Microsoft-Windows-USB-UCX 0x141 0xff
if errorlevel 1 goto fail
logman update trace usbtrace -ets -p Microsoft-Windows-USB-USBXHCI 0x141 0xff
logman update trace usbtrace -ets -p Microsoft-Windows-USB-USBHUB3 0x41 0xff

echo.
echo === trace started ===
if "%SERIAL%"=="" goto norestart
echo Restarting unit %SERIAL% so the driver start-up (FW boot, auth, I2C) is captured...
pnputil /restart-device "USB\VID_06B8&PID_103B\%SERIAL%"
goto started
:norestart
echo No serial given: no unit is restarted, so the driver start-up is not in the trace.
:started
echo.
echo Now: start StationTV X, watch a terrestrial channel for about 10 seconds,
echo then close StationTV X and press any key here to stop the trace.
pause >nul

logman stop usbtrace -ets
echo.
echo saved: %OUT%
goto end

:fail
echo logman failed - run this from an Administrator command prompt.
:end
endlocal
