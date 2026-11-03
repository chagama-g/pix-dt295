/*
 * IBonDriver2.h - BonDriver interface used by TVTest (IBonDriver / IBonDriver2).
 *
 * This is the binary interface TVTest calls through, so the declaration order
 * below IS the vtable layout: do not reorder, add or remove members. MSVC lays
 * out the two GetTsStream overloads in its own order; declaring them in the
 * same order as the original header keeps the layout identical.
 *
 * TVTest is a Unicode application, so the string type is LPCWSTR (the original
 * header writes LPCTSTR and is always built with UNICODE defined).
 */
#ifndef IBONDRIVER2_H
#define IBONDRIVER2_H

#include <windows.h>

class IBonDriver
{
public:
    virtual const BOOL OpenTuner(void) = 0;
    virtual void CloseTuner(void) = 0;

    virtual const BOOL SetChannel(const BYTE bCh) = 0;
    virtual const float GetSignalLevel(void) = 0;

    virtual const DWORD WaitTsStream(const DWORD dwTimeOut = 0) = 0;
    virtual const DWORD GetReadyCount(void) = 0;

    virtual const BOOL GetTsStream(BYTE *pDst, DWORD *pdwSize, DWORD *pdwRemain) = 0;
    virtual const BOOL GetTsStream(BYTE **ppDst, DWORD *pdwSize, DWORD *pdwRemain) = 0;

    virtual void PurgeTsStream(void) = 0;

    virtual void Release(void) = 0;
};

class IBonDriver2 : public IBonDriver
{
public:
    virtual LPCWSTR GetTunerName(void) = 0;

    virtual const BOOL IsTunerOpening(void) = 0;

    virtual LPCWSTR EnumTuningSpace(const DWORD dwSpace) = 0;
    virtual LPCWSTR EnumChannelName(const DWORD dwSpace, const DWORD dwChannel) = 0;

    virtual const BOOL SetChannel(const DWORD dwSpace, const DWORD dwChannel) = 0;

    virtual const DWORD GetCurSpace(void) = 0;
    virtual const DWORD GetCurChannel(void) = 0;

    virtual void Release(void) = 0;
};

#endif /* IBONDRIVER2_H */
