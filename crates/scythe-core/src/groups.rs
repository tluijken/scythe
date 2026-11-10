//! Track groups as REAPER keeps them: 64 groups, and per track one bit mask per grouped
//! parameter (`GROUP_FLAGS` holds groups 1-32, `GROUP_FLAGS_HIGH` groups 33-64). A lead's
//! edits carry over to the followers in the same group: volume as a dB offset, pan and width
//! as an offset, mute/solo/arm/polarity/automation mode as the same state. VCA leads do not
//! move their followers' faders; the engine multiplies their levels into the followers.

use crate::model::{AutoMode, Project};

/// The grouped parameters in `GROUP_FLAGS` order, with REAPER's names for them (as
/// `GetSetTrackGroupMembership` takes them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupParam {
    VolumeLead,
    VolumeFollow,
    PanLead,
    PanFollow,
    MuteLead,
    MuteFollow,
    SoloLead,
    SoloFollow,
    RecArmLead,
    RecArmFollow,
    PolarityLead,
    PolarityFollow,
    AutomodeLead,
    AutomodeFollow,
    VolumeReverse,
    PanReverse,
    NoLeadWhenFollow,
    WidthReverse,
    WidthLead,
    WidthFollow,
    VcaLead,
    VcaFollow,
    VcaFollowPreFx,
    MediaEditLead,
    MediaEditFollow,
}

impl GroupParam {
    pub const ALL: [GroupParam; 25] = [
        Self::VolumeLead,
        Self::VolumeFollow,
        Self::PanLead,
        Self::PanFollow,
        Self::MuteLead,
        Self::MuteFollow,
        Self::SoloLead,
        Self::SoloFollow,
        Self::RecArmLead,
        Self::RecArmFollow,
        Self::PolarityLead,
        Self::PolarityFollow,
        Self::AutomodeLead,
        Self::AutomodeFollow,
        Self::VolumeReverse,
        Self::PanReverse,
        Self::NoLeadWhenFollow,
        Self::WidthReverse,
        Self::WidthLead,
        Self::WidthFollow,
        Self::VcaLead,
        Self::VcaFollow,
        Self::VcaFollowPreFx,
        Self::MediaEditLead,
        Self::MediaEditFollow,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::VolumeLead => "VOLUME_LEAD",
            Self::VolumeFollow => "VOLUME_FOLLOW",
            Self::PanLead => "PAN_LEAD",
            Self::PanFollow => "PAN_FOLLOW",
            Self::MuteLead => "MUTE_LEAD",
            Self::MuteFollow => "MUTE_FOLLOW",
            Self::SoloLead => "SOLO_LEAD",
            Self::SoloFollow => "SOLO_FOLLOW",
            Self::RecArmLead => "RECARM_LEAD",
            Self::RecArmFollow => "RECARM_FOLLOW",
            Self::PolarityLead => "POLARITY_LEAD",
            Self::PolarityFollow => "POLARITY_FOLLOW",
            Self::AutomodeLead => "AUTOMODE_LEAD",
            Self::AutomodeFollow => "AUTOMODE_FOLLOW",
            Self::VolumeReverse => "VOLUME_REVERSE",
            Self::PanReverse => "PAN_REVERSE",
            Self::NoLeadWhenFollow => "NO_LEAD_WHEN_FOLLOW",
            Self::WidthReverse => "WIDTH_REVERSE",
            Self::WidthLead => "WIDTH_LEAD",
            Self::WidthFollow => "WIDTH_FOLLOW",
            Self::VcaLead => "VOLUME_VCA_LEAD",
            Self::VcaFollow => "VOLUME_VCA_FOLLOW",
            Self::VcaFollowPreFx => "VOLUME_VCA_FOLLOW_ISPREFX",
            Self::MediaEditLead => "MEDIA_EDIT_LEAD",
            Self::MediaEditFollow => "MEDIA_EDIT_FOLLOW",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(name))
    }

    /// Short label for the group matrix.
    pub fn label(self) -> &'static str {
        match self {
            Self::VolumeLead => "Volume lead",
            Self::VolumeFollow => "Volume follow",
            Self::PanLead => "Pan lead",
            Self::PanFollow => "Pan follow",
            Self::MuteLead => "Mute lead",
            Self::MuteFollow => "Mute follow",
            Self::SoloLead => "Solo lead",
            Self::SoloFollow => "Solo follow",
            Self::RecArmLead => "Arm lead",
            Self::RecArmFollow => "Arm follow",
            Self::PolarityLead => "Polarity lead",
            Self::PolarityFollow => "Polarity follow",
            Self::AutomodeLead => "Automation lead",
            Self::AutomodeFollow => "Automation follow",
            Self::VolumeReverse => "Volume reverse",
            Self::PanReverse => "Pan reverse",
            Self::NoLeadWhenFollow => "No lead when following",
            Self::WidthReverse => "Width reverse",
            Self::WidthLead => "Width lead",
            Self::WidthFollow => "Width follow",
            Self::VcaLead => "VCA lead",
            Self::VcaFollow => "VCA follow",
            Self::VcaFollowPreFx => "VCA pre-FX follow",
            Self::MediaEditLead => "Media edit lead",
            Self::MediaEditFollow => "Media edit follow",
        }
    }
}

pub const GROUP_COUNT: usize = 64;

/// A track's group memberships: per parameter, bit `g` set for group `g + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupFlags(pub [u64; 25]);

impl GroupFlags {
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|m| *m == 0)
    }

    pub fn mask(&self, p: GroupParam) -> u64 {
        self.0[p.index()]
    }

    /// Whether the track has `p` in group `group` (0-based).
    pub fn has(&self, p: GroupParam, group: usize) -> bool {
        group < GROUP_COUNT && self.mask(p) & (1 << group) != 0
    }

    pub fn set(&mut self, p: GroupParam, group: usize, on: bool) {
        if group >= GROUP_COUNT {
            return;
        }
        let m = &mut self.0[p.index()];
        if on {
            *m |= 1 << group;
        } else {
            *m &= !(1 << group);
        }
    }

    /// Groups (0-based) the track is in for any parameter.
    pub fn groups(&self) -> u64 {
        self.0.iter().fold(0, |a, m| a | m)
    }

    /// Merges one `GROUP_FLAGS` (`high` false) or `GROUP_FLAGS_HIGH` line's values.
    pub fn read_line(&mut self, values: &[String], high: bool) {
        for (i, v) in values.iter().take(25).enumerate() {
            let bits = v.parse::<i64>().unwrap_or(0) as u32 as u64;
            let shift = if high { 32 } else { 0 };
            self.0[i] = (self.0[i] & !(0xffff_ffff << shift)) | (bits << shift);
        }
    }

    /// The values of the `GROUP_FLAGS` (`high` false) or `GROUP_FLAGS_HIGH` line, without
    /// trailing zeros as REAPER writes them; empty when the line has nothing to say.
    pub fn line_values(&self, high: bool) -> Vec<String> {
        let shift = if high { 32 } else { 0 };
        let mut v: Vec<u32> = self.0.iter().map(|m| (m >> shift) as u32).collect();
        while v.last() == Some(&0) {
            v.pop();
        }
        v.into_iter().map(|b| b.to_string()).collect()
    }
}

/// A control whose edits carry over to the followers of the track's groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupedControl {
    Volume,
    Pan,
    Width,
    Mute,
    Solo,
    RecArm,
    Polarity,
    Automode,
}

impl GroupedControl {
    fn params(self) -> (GroupParam, GroupParam, Option<GroupParam>) {
        use GroupParam::*;
        match self {
            Self::Volume => (VolumeLead, VolumeFollow, Some(VolumeReverse)),
            Self::Pan => (PanLead, PanFollow, Some(PanReverse)),
            Self::Width => (WidthLead, WidthFollow, Some(WidthReverse)),
            Self::Mute => (MuteLead, MuteFollow, None),
            Self::Solo => (SoloLead, SoloFollow, None),
            Self::RecArm => (RecArmLead, RecArmFollow, None),
            Self::Polarity => (PolarityLead, PolarityFollow, None),
            Self::Automode => (AutomodeLead, AutomodeFollow, None),
        }
    }
}

/// The top of REAPER's fader: grouped volume changes stop here.
const MAX_VOLUME: f64 = 3.981_071_705_534_973;

impl Project {
    /// Tracks (by index) that follow an edit of `control` on track `lead`, with whether each
    /// moves the other way (the reverse flag). Followers that lead other groups pass the
    /// edit on unless they are marked "no lead when following".
    pub fn group_followers(&self, lead: usize, control: GroupedControl) -> Vec<(usize, bool)> {
        let (lead_p, follow_p, reverse_p) = control.params();
        let mut out: Vec<(usize, bool)> = Vec::new();
        let mut queue = vec![(lead, false)];
        let mut seen = vec![false; self.tracks.len()];
        if let Some(s) = seen.get_mut(lead) {
            *s = true;
        }
        while let Some((t, reversed)) = queue.pop() {
            let Some(track) = self.tracks.get(t) else { continue };
            if t != lead && track.groups.mask(GroupParam::NoLeadWhenFollow) != 0 {
                continue;
            }
            let groups = track.groups.mask(lead_p);
            if groups == 0 {
                continue;
            }
            for (u, other) in self.tracks.iter().enumerate() {
                let shared = other.groups.mask(follow_p) & groups;
                if seen[u] || shared == 0 {
                    continue;
                }
                seen[u] = true;
                let flip = reverse_p.is_some_and(|r| other.groups.mask(r) & shared != 0);
                out.push((u, reversed ^ flip));
                queue.push((u, reversed ^ flip));
            }
        }
        out
    }

    /// Sets a track's volume and moves its group followers by the same number of dB (the
    /// other way for reversed ones), capped at +12 dB.
    pub fn set_volume_grouped(&mut self, track: usize, volume: f64) {
        let Some(old) = self.tracks.get(track).map(|t| t.volume) else { return };
        let followers = self.group_followers(track, GroupedControl::Volume);
        self.tracks[track].volume = volume;
        for (u, reverse) in followers {
            let t = &mut self.tracks[u];
            t.volume = if old <= 0.0 || volume <= 0.0 {
                if reverse { t.volume } else { volume }
            } else {
                let ratio = volume / old;
                (t.volume * if reverse { 1.0 / ratio } else { ratio }).min(MAX_VOLUME)
            };
        }
    }

    /// Sets a track's pan and offsets its group followers' pans by the same amount.
    pub fn set_pan_grouped(&mut self, track: usize, pan: f64) {
        let Some(old) = self.tracks.get(track).map(|t| t.pan) else { return };
        let followers = self.group_followers(track, GroupedControl::Pan);
        self.tracks[track].pan = pan;
        let delta = pan - old;
        for (u, reverse) in followers {
            let t = &mut self.tracks[u];
            t.pan = (t.pan + if reverse { -delta } else { delta }).clamp(-1.0, 1.0);
        }
    }

    /// Sets a track's stereo width and offsets its group followers' widths.
    pub fn set_width_grouped(&mut self, track: usize, width: f64) {
        let Some(old) = self.tracks.get(track).map(|t| t.width) else { return };
        let followers = self.group_followers(track, GroupedControl::Width);
        self.tracks[track].width = width;
        let delta = width - old;
        for (u, reverse) in followers {
            let t = &mut self.tracks[u];
            t.width = (t.width + if reverse { -delta } else { delta }).clamp(-1.0, 1.0);
        }
    }

    /// Sets a track's volume and moves the other selected tracks (`peers`) by the same number
    /// of dB, as REAPER's faders do on a selection; everyone's group followers follow.
    pub fn set_volume_selected(&mut self, track: usize, volume: f64, peers: &[usize]) {
        let Some(old) = self.tracks.get(track).map(|t| t.volume) else { return };
        self.set_volume_grouped(track, volume);
        let n = self.tracks.len();
        for &u in peers.iter().filter(|&&u| u != track && u < n) {
            let v = self.tracks[u].volume;
            let next = if old <= 0.0 || volume <= 0.0 { volume } else { (v * volume / old).min(MAX_VOLUME) };
            self.set_volume_grouped(u, next);
        }
    }

    /// Sets a track's pan and offsets the other selected tracks' pans by the same amount.
    pub fn set_pan_selected(&mut self, track: usize, pan: f64, peers: &[usize]) {
        let Some(old) = self.tracks.get(track).map(|t| t.pan) else { return };
        self.set_pan_grouped(track, pan);
        let n = self.tracks.len();
        for &u in peers.iter().filter(|&&u| u != track && u < n) {
            let next = (self.tracks[u].pan + pan - old).clamp(-1.0, 1.0);
            self.set_pan_grouped(u, next);
        }
    }

    /// Applies `f` to a track and to the followers of its `control` groups.
    fn set_grouped(&mut self, track: usize, control: GroupedControl, f: impl Fn(&mut crate::Track)) {
        if track >= self.tracks.len() {
            return;
        }
        let followers = self.group_followers(track, control);
        f(&mut self.tracks[track]);
        for (u, _) in followers {
            f(&mut self.tracks[u]);
        }
    }

    pub fn set_mute_grouped(&mut self, track: usize, muted: bool) {
        self.set_grouped(track, GroupedControl::Mute, |t| t.muted = muted);
    }

    pub fn set_solo_grouped(&mut self, track: usize, solo: bool) {
        self.set_grouped(track, GroupedControl::Solo, |t| t.solo = solo);
    }

    pub fn set_arm_grouped(&mut self, track: usize, armed: bool) {
        self.set_grouped(track, GroupedControl::RecArm, |t| t.armed = armed);
    }

    pub fn set_phase_grouped(&mut self, track: usize, phase: bool) {
        self.set_grouped(track, GroupedControl::Polarity, |t| t.phase = phase);
    }

    pub fn set_automode_grouped(&mut self, track: usize, mode: AutoMode) {
        self.set_grouped(track, GroupedControl::Automode, |t| t.auto_mode = mode);
    }

    /// The VCA leads whose levels act on a track, nearest first, each with whether the track
    /// follows it pre-FX. Leads of leads count too.
    pub fn vca_leads(&self, track: usize) -> Vec<(usize, bool)> {
        let mut out: Vec<(usize, bool)> = Vec::new();
        let mut queue = vec![(track, false)];
        let mut seen = vec![false; self.tracks.len()];
        if let Some(s) = seen.get_mut(track) {
            *s = true;
        }
        while let Some((t, pre_fx)) = queue.pop() {
            let flags = &self.tracks[t].groups;
            let follows = flags.mask(GroupParam::VcaFollow);
            if follows == 0 {
                continue;
            }
            for (u, other) in self.tracks.iter().enumerate() {
                let shared = other.groups.mask(GroupParam::VcaLead) & follows;
                if seen[u] || shared == 0 {
                    continue;
                }
                seen[u] = true;
                let pre = pre_fx || (t == track && flags.mask(GroupParam::VcaFollowPreFx) & shared != 0);
                out.push((u, pre));
                queue.push((u, pre));
            }
        }
        out
    }

    /// A group's name (0-based index), when it has one.
    pub fn group_name(&self, group: usize) -> Option<&str> {
        self.group_names.get(&group).map(String::as_str)
    }

    /// Groups (0-based) that have a name or any member.
    pub fn used_groups(&self) -> Vec<usize> {
        let members = self.tracks.iter().fold(0u64, |a, t| a | t.groups.groups());
        (0..GROUP_COUNT).filter(|g| members & (1 << g) != 0 || self.group_names.contains_key(g)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Track;

    fn project(n: usize) -> Project {
        let mut p = Project::default();
        for i in 0..n {
            p.tracks.push(Track::new(format!("t{i}")));
        }
        p
    }

    #[test]
    fn flags_lines_round_trip() {
        let mut f = GroupFlags::default();
        f.set(GroupParam::VolumeLead, 0, true);
        f.set(GroupParam::MuteLead, 39, true);
        f.set(GroupParam::VcaFollow, 1, true);
        assert_eq!(f.line_values(false).join(" "), "1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 2");
        assert_eq!(f.line_values(true).join(" "), "0 0 0 0 128");
        let mut g = GroupFlags::default();
        g.read_line(&f.line_values(false), false);
        g.read_line(&f.line_values(true), true);
        assert_eq!(f, g);
        assert_eq!(GroupParam::from_name("volume_vca_follow_isprefx"), Some(GroupParam::VcaFollowPreFx));
        assert_eq!(GroupParam::MediaEditFollow.index(), 24);
    }

    // Measured with CSurf_OnVolumeChangeEx / CSurf_OnPanChangeEx on group leads in REAPER 7.73.
    #[test]
    fn edits_carry_over_like_reaper() {
        let mut p = project(4);
        p.tracks[0].groups.set(GroupParam::VolumeLead, 0, true);
        p.tracks[1].groups.set(GroupParam::VolumeFollow, 0, true);
        p.tracks[2].groups.set(GroupParam::VolumeFollow, 0, true);
        p.tracks[2].groups.set(GroupParam::VolumeReverse, 0, true);
        (p.tracks[1].volume, p.tracks[2].volume, p.tracks[3].volume) = (0.5, 0.5, 0.7);
        p.set_volume_grouped(0, 0.5);
        let v: Vec<f64> = p.tracks.iter().map(|t| t.volume).collect();
        assert_eq!(v, [0.5, 0.25, 1.0, 0.7]);
        p.set_volume_grouped(0, 4.0);
        assert!((p.tracks[1].volume - 2.0).abs() < 1e-9 && (p.tracks[2].volume - 0.125).abs() < 1e-9);
        p.set_volume_grouped(0, 2.0);
        p.set_volume_grouped(0, 4.0);
        p.tracks[1].volume = 1.0;
        p.set_volume_grouped(0, 16.0);
        assert!((p.tracks[1].volume - MAX_VOLUME).abs() < 1e-9, "capped at +12 dB");

        let mut p = project(3);
        p.tracks[0].groups.set(GroupParam::PanLead, 0, true);
        p.tracks[1].groups.set(GroupParam::PanFollow, 0, true);
        p.tracks[2].groups.set(GroupParam::PanFollow, 0, true);
        p.tracks[2].groups.set(GroupParam::PanReverse, 0, true);
        p.tracks[1].pan = 0.2;
        p.tracks[2].pan = 0.2;
        p.set_pan_grouped(0, 0.5);
        assert!((p.tracks[1].pan - 0.7).abs() < 1e-9 && (p.tracks[2].pan + 0.3).abs() < 1e-9);

        let mut p = project(3);
        p.tracks[0].groups.set(GroupParam::MuteLead, 0, true);
        p.tracks[1].groups.set(GroupParam::MuteFollow, 0, true);
        p.set_mute_grouped(0, true);
        assert!(p.tracks[1].muted && !p.tracks[2].muted);
        p.set_mute_grouped(1, false);
        assert!(p.tracks[0].muted && !p.tracks[1].muted, "followers do not lead");
    }

    #[test]
    fn edits_chain_through_leads_of_other_groups() {
        let mut p = project(3);
        p.tracks[0].groups.set(GroupParam::VolumeLead, 0, true);
        p.tracks[1].groups.set(GroupParam::VolumeFollow, 0, true);
        p.tracks[1].groups.set(GroupParam::VolumeLead, 1, true);
        p.tracks[2].groups.set(GroupParam::VolumeFollow, 1, true);
        p.set_volume_grouped(0, 0.5);
        assert!(p.tracks.iter().all(|t| t.volume == 0.5));
        p.tracks[1].groups.set(GroupParam::NoLeadWhenFollow, 1, true);
        p.set_volume_grouped(0, 0.25);
        assert_eq!((p.tracks[1].volume, p.tracks[2].volume), (0.25, 0.5));
    }

    #[test]
    fn vca_leads_nest() {
        let mut p = project(3);
        p.tracks[0].groups.set(GroupParam::VcaLead, 1, true);
        p.tracks[1].groups.set(GroupParam::VcaFollow, 1, true);
        p.tracks[1].groups.set(GroupParam::VcaLead, 0, true);
        p.tracks[2].groups.set(GroupParam::VcaFollow, 0, true);
        p.tracks[2].groups.set(GroupParam::VcaFollowPreFx, 0, true);
        assert_eq!(p.vca_leads(2), vec![(1, true), (0, true)]);
        assert_eq!(p.vca_leads(1), vec![(0, false)]);
        assert!(p.vca_leads(0).is_empty());
    }

    #[test]
    fn selected_tracks_move_together() {
        let mut p = project(3);
        p.tracks[1].volume = 0.5;
        p.tracks[2].pan = 0.9;
        p.set_volume_selected(0, 2.0, &[0, 1]);
        assert_eq!((p.tracks[0].volume, p.tracks[1].volume, p.tracks[2].volume), (2.0, 1.0, 1.0));
        p.set_pan_selected(0, 0.5, &[0, 2]);
        assert_eq!((p.tracks[0].pan, p.tracks[2].pan), (0.5, 1.0));
    }
}
