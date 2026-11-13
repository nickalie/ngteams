//! Native UI Automation client on its own MTA. Only immediate desktop children and verified toast trees are searched.
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{E_FAIL, RECT},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            Variant::VARIANT,
        },
        UI::Accessibility::{
            CUIAutomation8, IUIAutomation6, IUIAutomationElement, IUIAutomationInvokePattern,
            PropertyConditionFlags_MatchSubstring, TreeScope_Children, TreeScope_Descendants, UIA_InvokePatternId,
            UIA_NamePropertyId,
        },
    },
    core::Result,
};

pub(super) async fn click(title: &'static str, action: &'static str) {
    let (reply, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = reply.send(drive(title, action));
    });
    result.await.unwrap().expect("native toast activation driver");
}
fn client() -> Result<IUIAutomation6> {
    // SAFETY: this window-free MTA owns the client and bounds each provider call in milliseconds.
    let client: IUIAutomation6 = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }?;
    // SAFETY: valid owned client, milliseconds; only this window-free MTA uses it.
    unsafe { client.SetConnectionTimeout(500) }?;
    // SAFETY: the same owned client and finite millisecond transaction deadline.
    unsafe { client.SetTransactionTimeout(1000) }?;
    Ok(client)
}
fn roots(client: &IUIAutomation6) -> Result<Vec<IUIAutomationElement>> {
    // Ask UIA for toast roots: Win32 HWND enumeration did not reliably discover this shell's banners.
    // Never search desktop descendants.
    // SAFETY: the owned client returns a retained root, without traversing descendants.
    let desktop = unsafe { client.GetRootElement() }?;
    let condition = named(client, "New notification")?;
    // SAFETY: fixed-name condition, searched only among immediate desktop children.
    let items = unsafe { desktop.FindAll(TreeScope_Children, &condition) }?;
    // SAFETY: the collection remains retained on this thread.
    let length = unsafe { items.Length() }?;
    (0..length)
        .map(|i| {
            // SAFETY: the index is inside the collection's reported length.
            unsafe { items.GetElement(i) }
        })
        .collect()
}
fn named(client: &IUIAutomation6, label: &str) -> Result<windows::Win32::UI::Accessibility::IUIAutomationCondition> {
    // SAFETY: the immutable condition owns a copy of the label.
    unsafe {
        client.CreatePropertyConditionEx(
            UIA_NamePropertyId,
            &VARIANT::from(label),
            PropertyConditionFlags_MatchSubstring,
        )
    }
}
fn find(client: &IUIAutomation6, root: &IUIAutomationElement, label: &str) -> Result<IUIAutomationElement> {
    let condition = named(client, label)?;
    // SAFETY: searches only a retained shell toast or one of its synthetic title's ancestors.
    unsafe { root.FindFirst(TreeScope_Descendants, &condition) }
}
fn visible(element: &IUIAutomationElement) -> Result<bool> {
    // SAFETY: a retained element used only on this client's owning MTA.
    unsafe { element.CurrentIsOffscreen() }.map(|value| !value.as_bool())
}
fn rectangle(element: &IUIAutomationElement) -> Result<RECT> {
    // SAFETY: a retained element used only on this client's owning MTA.
    unsafe { element.CurrentBoundingRectangle() }
}
fn pattern(element: &IUIAutomationElement) -> Result<IUIAutomationInvokePattern> {
    // SAFETY: query only, no action. The retained element belongs to the verified synthetic toast.
    unsafe { element.GetCurrentPatternAs(UIA_InvokePatternId) }
}

fn drive(title: &str, action: &str) -> Result<()> {
    let _apartment = super::Apartment::new()?;
    let client = client()?;
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut stable: Option<((i32, i32, i32, i32), Instant)> = None;
    while Instant::now() < deadline {
        for root in roots(&client)? {
            let Ok((target, bounds)) = target(&client, &root, title, action) else {
                continue;
            };
            let key = (bounds.left, bounds.top, bounds.right, bounds.bottom);
            if let Some((previous, since)) = stable
                && previous == key
            {
                if since.elapsed() >= Duration::from_millis(250) {
                    screenshot(&bounds, action);
                    let pattern = pattern(&target)?;
                    // SAFETY: the action belongs to the visible synthetic title in this settled toast.
                    return unsafe { pattern.Invoke() };
                }
            } else {
                stable = Some((key, Instant::now()));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(windows::core::Error::from_hresult(E_FAIL))
}
fn target(
    client: &IUIAutomation6,
    root: &IUIAutomationElement,
    title: &str,
    action: &str,
) -> Result<(IUIAutomationElement, RECT)> {
    let text = find(client, root, title)?;
    if !visible(&text)? {
        return Err(E_FAIL.into());
    }
    let text_bounds = rectangle(&text)?;
    // SAFETY: the owning client returns a retained walker used only on this thread.
    let walker = unsafe { client.ControlViewWalker() }?;
    let mut parent = text;
    for _ in 0..7 {
        let bounds = rectangle(&parent)?;
        if bounded(&bounds) && contains(&bounds, &text_bounds) {
            let target = if action == "default" {
                Some(parent.clone())
            } else {
                let button = match action {
                    "answer" => "Answer",
                    "join" => "Join",
                    _ => "Decline",
                };
                find(client, &parent, button).ok()
            };
            if let Some(target) = target
                && visible(&target)?
                && contains(&bounds, &rectangle(&target)?)
                && pattern(&target).is_ok()
            {
                return Ok((target, bounds));
            }
        }
        // SAFETY: move within the retained synthetic title's ancestry, without an action.
        parent = unsafe { walker.GetParentElement(&parent) }?;
    }
    Err(E_FAIL.into())
}

fn bounded(bounds: &RECT) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    let (left, top, width, height) = (
        metric(SM_XVIRTUALSCREEN),
        metric(SM_YVIRTUALSCREEN),
        metric(SM_CXVIRTUALSCREEN),
        metric(SM_CYVIRTUALSCREEN),
    );
    (200..=700).contains(&(bounds.right - bounds.left))
        && (70..=450).contains(&(bounds.bottom - bounds.top))
        && bounds.left >= left
        && bounds.top >= top
        && bounds.right <= left + width
        && bounds.bottom <= top + height
}
fn metric(index: i32) -> i32 {
    // SAFETY: reads a fixed virtual-screen metric, without pointer arguments or side effects.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(index) }
}

fn contains(outer: &RECT, inner: &RECT) -> bool {
    inner.left >= outer.left && inner.top >= outer.top && inner.right <= outer.right && inner.bottom <= outer.bottom
}
pub(super) async fn closed(title: &'static str) {
    let (reply, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = reply.send(wait_closed(title));
    });
    result.await.unwrap().expect("synthetic toast withdrawal");
}
fn wait_closed(title: &str) -> Result<()> {
    let _apartment = super::Apartment::new()?;
    let client = client()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let found = roots(&client)?.iter().any(|root| find(&client, root, title).is_ok());
        if !found {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(E_FAIL.into())
}
fn screenshot(bounds: &RECT, action: &str) {
    let root = PathBuf::from(std::env::var_os("UI_SANDBOX_DIR").expect("fixture evidence directory"));
    let path = root.join(format!("{action}.rectangle.json"));
    std::fs::write(
        &path,
        serde_json::json!({"left":bounds.left,"top":bounds.top,"right":bounds.right,"bottom":bounds.bottom})
            .to_string(),
    )
    .unwrap();
    let ack = root.join(format!("{action}.captured"));
    let until = Instant::now() + Duration::from_secs(5);
    while !ack.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ack.exists(),
        "console helper did not capture the verified synthetic toast"
    );
}
