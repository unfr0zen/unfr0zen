use objc2::{define_class, sel, ClassType, MainThreadMarker, MainThreadOnly};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSApplication, NSImage, NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName,
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits, NSAboutPanelOptionVersion,
};
use objc2_foundation::{ns_string, NSAttributedString, NSDictionary, NSObject, NSString};

define_class!(
    // NSObject has no subclassing requirements; this class owns no instances.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "Unfr0zenAboutMenu"]
    struct AboutMenu;

    impl AboutMenu {
        #[unsafe(method(showAbout:))]
        fn show_about(_sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::new().expect("AppKit menu actions run on the main thread");
            let app = NSApplication::sharedApplication(mtm);
            let icon = app.applicationIconImage().expect("eframe sets the application icon");
            // Each option has the type required by AppKit's standard About panel.
            unsafe { app.orderFrontStandardAboutPanelWithOptions(&options(&icon)) };
        }
    }
);

pub fn install() {
    let mtm = MainThreadMarker::new().expect("eframe creates the app on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let menu = app.mainMenu().and_then(|menu| menu.itemAtIndex(0)).and_then(|item| item.submenu())
        .expect("winit creates the application menu before the app");
    for item in menu.itemArray() {
        if item.action() == Some(sel!(orderFrontStandardAboutPanel:)) {
            item.setTitle(ns_string!("About unfr0zen"));
            // The class object lives for the process lifetime and implements
            // this action with AppKit's target/action signature.
            unsafe {
                item.setTarget(Some(AboutMenu::class().as_ref()));
                item.setAction(Some(sel!(showAbout:)));
            }
            break;
        }
    }
}

fn options(icon: &NSImage) -> Retained<NSDictionary<NSString, AnyObject>> {
    let name = NSString::from_str("unfr0zen");
    let version = NSString::from_str(crate::BUILD_VERSION);
    let credits = NSAttributedString::from_nsstring(ns_string!(""));
    // These exported keys describe NSString, NSImage and NSAttributedString
    // values respectively; all objects are retained by the dictionary.
    unsafe {
        NSDictionary::from_slices(
            &[NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion,
              NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionCredits, NSAboutPanelOptionVersion,
              ns_string!("Copyright")],
            &[&*name as &AnyObject, &*version, icon, &*credits, ns_string!(""), ns_string!("")],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::AnyThread;
    use objc2_foundation::NSData;

    #[test]
    fn native_panel_has_version_icon_and_no_credits() {
        let data = NSData::with_bytes(include_bytes!("../../../assets/icon/unfr0zen-macos.png"));
        let icon = NSImage::initWithData(NSImage::alloc(), &data).expect("app icon decodes in AppKit");
        assert!(icon.size().width > 0.0 && icon.size().height > 0.0);
        let panel = options(&icon);
        let name = panel.objectForKey(ns_string!("ApplicationName")).unwrap();
        assert_eq!(name.downcast_ref::<NSString>().unwrap().to_string(), "unfr0zen");
        let version = panel.objectForKey(ns_string!("ApplicationVersion")).unwrap();
        assert_eq!(version.downcast_ref::<NSString>().unwrap().to_string(), crate::BUILD_VERSION);
        let image = panel.objectForKey(ns_string!("ApplicationIcon")).unwrap();
        assert!(image.downcast_ref::<NSImage>().is_some());
        let credits = panel.objectForKey(ns_string!("Credits")).unwrap();
        let text = credits.downcast_ref::<NSAttributedString>().unwrap().string().to_string();
        assert!(text.is_empty());
    }
}
