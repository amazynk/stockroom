//! Copies view models into the generated Slint structs, and keeps list models in place so
//! elements (and keyboard focus) survive updates.

use std::rc::Rc;

use slint::{Image, Model, ModelRc, SharedString, VecModel};
use suite_ui_markup::ui;

use crate::{
    jobs::{JobView, State},
    view::{self, Act, CardView, LogView, PageView, Tone},
};

pub(crate) fn tone(t: Tone) -> ui::Tone {
    match t {
        Tone::Normal => ui::Tone::Normal,
        Tone::Muted => ui::Tone::Muted,
        Tone::Ok => ui::Tone::Ok,
        Tone::Warn => ui::Tone::Warn,
        Tone::Bad => ui::Tone::Bad,
        Tone::Accent => ui::Tone::Accent,
    }
}

pub(crate) fn action(a: Act) -> ui::Action {
    match a {
        Act::None => ui::Action::None,
        Act::Install => ui::Action::Install,
        Act::Open => ui::Action::Open,
        Act::Update => ui::Action::Update,
        Act::Store(suite_catalog::Store::Website) => ui::Action::Website,
        Act::Store(_) => ui::Action::Store,
        Act::Busy => ui::Action::Busy,
    }
}

pub(crate) fn card(c: &CardView, icon: Option<&Image>) -> ui::CardData {
    ui::CardData {
        id: c.id.as_str().into(),
        part: c.part.as_str().into(),
        name: c.name.as_str().into(),
        pitch: c.pitch.as_str().into(),
        vendor: c.vendor.as_str().into(),
        initial: view::initial(&c.name).into(),
        tint: view::tint(&c.id),
        icon: icon.cloned().unwrap_or_default(),
        has_icon: icon.is_some(),
        status: c.status.as_str().into(),
        tone: tone(c.tone),
        action: action(c.act),
        action_text: c.act_text.as_str().into(),
        progress: c.progress,
    }
}

pub(crate) fn job(j: &JobView) -> ui::JobData {
    ui::JobData {
        id: i32::try_from(j.id).unwrap_or(i32::MAX),
        app: j.app.as_str().into(),
        initial: view::initial(&j.app).into(),
        tint: view::tint(&j.app),
        title: j.title.as_str().into(),
        stage: j.stage.as_str().into(),
        fraction: j.fraction,
        detail: j.detail.as_str().into(),
        state: match j.state {
            State::Queued => 0,
            State::Running => 1,
            State::Done => 2,
            State::Failed => 3,
        },
        message: j.message.as_str().into(),
        error_detail: j.error_detail.as_str().into(),
        can_open: j.can_open,
        can_retry: j.can_retry,
    }
}

pub(crate) fn log(l: &LogView) -> ui::LogData {
    ui::LogData { when: l.when.as_str().into(), text: l.text.as_str().into(), tone: tone(l.tone) }
}

/// Lists inside the page, kept between renders.
#[derive(Default)]
pub(crate) struct PageModels {
    facts: Rc<VecModel<ui::Fact>>,
    reqs: Rc<VecModel<ui::ReqLine>>,
    others: Rc<VecModel<ui::OtherInstall>>,
    phones: Rc<VecModel<ui::PhoneCode>>,
}

impl PageModels {
    /// `qr` makes the image for a phone link.
    pub(crate) fn page(
        &self,
        p: &PageView,
        icon: Option<&Image>,
        qr: &dyn Fn(&str) -> Image,
    ) -> ui::PageData {
        let c = p.card.clone().unwrap_or_else(|| CardView {
            id: String::new(),
            part: String::new(),
            name: String::new(),
            pitch: String::new(),
            vendor: String::new(),
            status: String::new(),
            tone: Tone::Normal,
            act: Act::None,
            act_text: String::new(),
            progress: -1.0,
        });
        sync(
            &self.facts,
            p.facts.iter().map(|(l, v)| ui::Fact { label: l.into(), value: v.into() }).collect(),
        );
        sync(&self.reqs, p.reqs.iter().map(|(t, ok)| ui::ReqLine { text: t.into(), ok: *ok }).collect());
        sync(
            &self.others,
            p.others
                .iter()
                .map(|o| ui::OtherInstall {
                    text: o.text.as_str().into(),
                    note: o.note.as_str().into(),
                    can_open: o.can_open,
                })
                .collect(),
        );
        sync(
            &self.phones,
            p.phones
                .iter()
                .map(|(label, url)| ui::PhoneCode { label: label.into(), url: url.into(), code: qr(url) })
                .collect(),
        );
        ui::PageData {
            id: c.id.as_str().into(),
            part: c.part.as_str().into(),
            name: c.name.as_str().into(),
            pitch: c.pitch.as_str().into(),
            vendor: c.vendor.as_str().into(),
            description: p.description.as_str().into(),
            initial: view::initial(&c.name).into(),
            tint: view::tint(&c.id),
            icon: icon.cloned().unwrap_or_default(),
            has_icon: icon.is_some(),
            status: c.status.as_str().into(),
            tone: tone(c.tone),
            action: action(c.act),
            action_text: c.act_text.as_str().into(),
            progress: c.progress,
            stage: p.stage.as_str().into(),
            facts: ModelRc::from(Rc::clone(&self.facts)),
            reqs: ModelRc::from(Rc::clone(&self.reqs)),
            problem: p.problem.as_str().into(),
            notes_url: p.notes_url.as_str().into(),
            homepage: p.homepage.as_str().into(),
            managed: p.managed,
            channel: i32::from(p.channel == suite_catalog::Channel::Beta),
            can_beta: p.can_beta,
            policy: view::policy_index(p.policy),
            rollback: p.rollback.as_str().into(),
            has_data: p.has_data,
            store_note: p.store_note.as_str().into(),
            others: ModelRc::from(Rc::clone(&self.others)),
            phones: ModelRc::from(Rc::clone(&self.phones)),
        }
    }
}

/// Makes `model` hold `items`, changing only the rows that differ.
pub(crate) fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    if model.row_count() != items.len() {
        model.set_vec(items);
        return;
    }
    for (i, item) in items.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&item) {
            model.set_row_data(i, item);
        }
    }
}

pub(crate) fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_changes_only_what_differs() {
        let m = VecModel::from(vec![1, 2, 3]);
        sync(&m, vec![1, 5, 3]);
        assert_eq!(m.iter().collect::<Vec<_>>(), [1, 5, 3]);
        sync(&m, vec![4]);
        assert_eq!(m.iter().collect::<Vec<_>>(), [4]);
    }
}
