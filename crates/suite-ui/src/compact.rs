//! `stockroom install <app> --ui`: a small window with one app's install and then Open. The
//! family's apps hand installs to Stockroom this way (PLAN.md §2). It runs on the same queue
//! as the app window, so both show the same job.

use std::rc::{Rc, Weak};

use slint::{ComponentHandle, ModelRc, PlatformError, VecModel};
use suite_catalog::Channel;
use suite_core::{CoreEvent, ErrorKind, Job};
use suite_ui_markup::ui;

use crate::{
    app::Inner,
    i18n::tr,
    jobs::{Jobs, State},
    look, view, words,
};

/// How many other apps the window offers.
pub(crate) const MORE: usize = 3;

pub(crate) struct Compact {
    pub ui: ui::InstallWindow,
    pub app: String,
    channel: Option<Channel>,
    job: Option<u64>,
    owner: Weak<Inner>,
    submitted: bool,
    /// The other apps offered under the install.
    more: Rc<VecModel<ui::CardData>>,
}

impl Compact {
    pub(crate) fn new(
        owner: Weak<Inner>,
        app: &str,
        channel: Option<Channel>,
    ) -> Result<Self, PlatformError> {
        let ui = ui::InstallWindow::new()?;
        look::follow(&ui);
        ui.set_product(suite_identity::NAME.into());
        ui.set_title_bar(crate::title_bar());
        let more = Rc::<VecModel<ui::CardData>>::default();
        ui.set_more(ModelRc::from(Rc::clone(&more)));
        let c = Self { ui, app: app.to_owned(), channel, job: None, owner, submitted: false, more };
        c.waiting();
        let me = c.owner.clone();
        c.ui.on_show_app(move |id| {
            if let Some(i) = me.upgrade() {
                i.show_app(&id);
            }
        });
        let me = c.owner.clone();
        c.ui.on_show_all(move || {
            if let Some(i) = me.upgrade() {
                i.show_all();
            }
        });
        let me = c.owner.clone();
        let app_id = c.app.clone();
        c.ui.on_open(move || {
            if let Some(i) = me.upgrade() {
                i.launch(&app_id);
                hide(&i);
            }
        });
        let me = c.owner.clone();
        c.ui.on_retry(move || {
            if let Some(i) = me.upgrade() {
                let job = i.compact.borrow_mut().as_mut().and_then(Compact::retry_job);
                if let Some(job) = job {
                    let id = i.submit_now(job);
                    if let Some(c) = i.compact.borrow_mut().as_mut() {
                        c.job = id;
                        c.submitted = id.is_some();
                        c.waiting();
                    }
                }
            }
        });
        let me = c.owner.clone();
        c.ui.on_dismiss(move || {
            if let Some(i) = me.upgrade() {
                hide(&i);
            }
        });
        let me = c.owner.clone();
        c.ui.window().on_close_requested(move || {
            if let Some(i) = me.upgrade() {
                slint::Timer::single_shot(std::time::Duration::ZERO, move || i.compact_closed());
            }
            slint::CloseRequestResponse::HideWindow
        });
        Ok(c)
    }

    fn name(&self) -> String {
        self.owner.upgrade().map_or_else(|| self.app.clone(), |i| i.name_of(&self.app))
    }

    fn waiting(&self) {
        let name = self.name();
        self.ui.set_name(name.as_str().into());
        self.ui.set_initial(view::initial(&name).into());
        self.ui.set_tint(view::tint(&self.app));
        if let Some(icon) = self.owner.upgrade().and_then(|i| i.icon(&self.app)) {
            self.ui.set_app_icon(icon);
            self.ui.set_has_icon(true);
        }
        self.ui.set_heading(tr!("Installing {}", name).into());
        self.ui.set_stage(tr!("Getting the app list").into());
        self.ui.set_fraction(-1.0);
        self.ui.set_detail("".into());
        self.ui.set_state(0);
    }

    pub(crate) fn show(&self) -> Result<(), PlatformError> {
        crate::show(&self.ui)
    }

    /// Offers the other apps (the rows or an icon changed).
    pub(crate) fn offer(&self, more: Vec<ui::CardData>) {
        if let Some(owner) = self.owner.upgrade() {
            self.ui.set_part(owner.part_of(&self.app).into());
            // The app's own icon may only have arrived now.
            if let Some(icon) = owner.icon(&self.app) {
                self.ui.set_app_icon(icon);
                self.ui.set_has_icon(true);
            }
        }
        crate::bind::sync(&self.more, more);
    }

    pub(crate) fn visible(&self) -> bool {
        self.ui.window().is_visible()
    }

    /// The (fresh) app list is in: the install starts.
    pub(crate) fn catalog_ready(&mut self) {
        if self.submitted {
            return;
        }
        let Some(owner) = self.owner.upgrade() else { return };
        self.job = owner.submit_now(Job::Install { app: self.app.clone(), channel: self.channel });
        self.submitted = self.job.is_some();
        if self.submitted {
            self.waiting();
        } else {
            self.no_catalog();
        }
    }

    pub(crate) fn no_catalog(&self) {
        if !self.submitted {
            self.failed(ErrorKind::Catalog, "");
        }
    }

    pub(crate) fn failed(&self, kind: ErrorKind, detail: &str) {
        if self.submitted {
            return;
        }
        self.ui.set_heading(tr!("{} wasn't installed", self.name()).into());
        self.ui.set_stage("".into());
        self.ui.set_message(words::catalog_problem(kind, false).into());
        self.ui.set_error_detail(detail.into());
        self.ui.set_state(2);
    }

    fn retry_job(&mut self) -> Option<Job> {
        self.submitted.then(|| Job::Install { app: self.app.clone(), channel: self.channel })
    }

    pub(crate) fn event(&mut self, ev: &CoreEvent, jobs: &Jobs) {
        if Some(ev.id()) == self.job {
            self.tick(jobs);
        }
    }

    /// Shows the job as the activity screen does.
    pub(crate) fn tick(&mut self, jobs: &Jobs) {
        let Some(view) = self.job.and_then(|id| jobs.views().into_iter().find(|v| v.id == id)) else {
            return;
        };
        let name = self.name();
        self.ui.set_name(name.as_str().into());
        self.ui.set_initial(view::initial(&name).into());
        self.ui.set_fraction(view.fraction);
        self.ui.set_detail(view.detail.as_str().into());
        self.ui.set_stage(view.stage.as_str().into());
        match view.state {
            State::Queued | State::Running => {
                self.ui.set_heading(tr!("Installing {}", name).into());
                self.ui.set_state(0);
            }
            State::Done if view.can_open => {
                self.ui.set_heading(view.title.as_str().into());
                self.ui.set_state(1);
            }
            State::Done => {
                // Handed to a store or the app's website: nothing to open from here.
                self.ui.set_heading(view.title.as_str().into());
                self.ui.set_stage("".into());
                self.ui.set_message(view.message.as_str().into());
                self.ui.set_state(3);
            }
            State::Failed => {
                self.ui.set_heading(tr!("{} wasn't installed", name).into());
                self.ui.set_stage("".into());
                self.ui.set_message(view.message.as_str().into());
                self.ui.set_error_detail(view.error_detail.as_str().into());
                self.ui.set_state(2);
            }
        }
    }
}

fn hide(i: &Inner) {
    if let Some(c) = i.compact.borrow().as_ref() {
        let _ = c.ui.hide();
    }
    i.compact_closed();
}
