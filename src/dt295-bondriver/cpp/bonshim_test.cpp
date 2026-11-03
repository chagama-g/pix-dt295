/*
 * bonshim_test.cpp - drives an IBonDriver the way TVTest does (dynamic_cast to
 * IBonDriver2, calls through the vtable, Release), for the crate's unit tests.
 *
 * Its own object file: only the test executable references it, so the linker never
 * pulls it into BonDriver_DT295.dll.
 */
#include "IBonDriver2.h"

/* Returns 1 if every check passed, else the (negative) number of the first that
 * failed. `name` receives the tuner name (NUL-terminated, at most `cap` units),
 * `spaces` the number of tuning spaces. Releases the driver in any case. */
extern "C" int bonshim_test_drive(IBonDriver *bon, wchar_t *name, int cap, DWORD *spaces) noexcept
{
    if (!bon) return -1;
    IBonDriver2 *b2 = dynamic_cast<IBonDriver2 *>(bon);
    if (!b2) { bon->Release(); return -2; }

    int rc = 1;
    LPCWSTR n = b2->GetTunerName();
    int i = 0;
    for (; n && n[i] && i + 1 < cap; i++) name[i] = n[i];
    if (cap > 0) name[i] = L'\0';

    DWORD s = 0;
    while (b2->EnumTuningSpace(s)) s++;
    *spaces = s;

    BYTE *p = reinterpret_cast<BYTE *>(1);
    DWORD size = 1, remain = 1;
    BYTE buf[16];
    if (rc == 1 && b2->IsTunerOpening()) rc = -3;
    if (rc == 1 && b2->GetCurSpace() != 0xFFFFFFFF) rc = -4;
    if (rc == 1 && b2->GetCurChannel() != 0xFFFFFFFF) rc = -5;
    if (rc == 1 && (!b2->GetTsStream(&p, &size, &remain) || p || size || remain)) rc = -6;
    if (rc == 1 && (!b2->GetTsStream(buf, &size, &remain) || size || remain)) rc = -7;
    if (rc == 1 && b2->GetReadyCount() != 0) rc = -8;
    if (rc == 1 && b2->WaitTsStream(1) != WAIT_TIMEOUT) rc = -9;
    if (rc == 1 && b2->SetChannel(0UL, 0UL)) rc = -10;      /* not open */
    if (rc == 1 && bon->SetChannel(static_cast<BYTE>(27))) rc = -11;  /* the v1 overload */
    if (rc == 1 && b2->EnumChannelName(0, 0) == NULL) rc = -12;
    if (rc == 1 && b2->EnumChannelName(0, 0xFFFF) != NULL) rc = -13;
    if (rc == 1 && b2->GetSignalLevel() != 0.0f) rc = -14;
    b2->PurgeTsStream();
    b2->CloseTuner();
    b2->Release();
    return rc;
}
