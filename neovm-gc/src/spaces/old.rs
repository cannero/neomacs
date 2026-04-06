use std::collections::{HashMap, HashSet};

use crate::object::{ObjectRecord, OldRegionPlacement, SpaceKind};
use crate::plan::{CollectionKind, CollectionPlan};
use crate::reclaim::PreparedReclaimSurvivor;
use crate::stats::OldRegionStats;

/// Old-generation configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OldGenConfig {
    /// Region size in bytes.
    pub region_bytes: usize,
    /// Line size in bytes for occupancy tracking.
    pub line_bytes: usize,
    /// Maximum number of old regions to target in one planned compaction cycle.
    pub compaction_candidate_limit: usize,
    /// Minimum reclaimable bytes required for a region to become a compaction candidate.
    pub selective_reclaim_threshold_bytes: usize,
    /// Maximum live bytes selected for compaction in one planned cycle.
    pub max_compaction_bytes_per_cycle: usize,
    /// Maximum number of concurrent mark workers.
    pub concurrent_mark_workers: usize,
    /// Number of major-mark slices one mutator operation should assist.
    pub mutator_assist_slices: usize,
}

impl Default for OldGenConfig {
    fn default() -> Self {
        Self {
            region_bytes: 4 * 1024 * 1024,
            line_bytes: 256,
            compaction_candidate_limit: 8,
            selective_reclaim_threshold_bytes: 1,
            max_compaction_bytes_per_cycle: usize::MAX,
            concurrent_mark_workers: 1,
            mutator_assist_slices: 1,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct OldGenState {
    pub(crate) regions: Vec<OldRegion>,
}

impl OldGenState {
    pub(crate) fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub(crate) fn reserved_bytes(&self) -> usize {
        self.regions
            .iter()
            .map(|region| region.capacity_bytes)
            .sum()
    }

    pub(crate) fn allocate_placement(
        &mut self,
        config: &OldGenConfig,
        bytes: usize,
    ) -> OldRegionPlacement {
        let align = config.line_bytes.max(8);
        if let Some((region_index, offset)) = self.try_reserve_in_existing_region(bytes, align) {
            return self.make_placement(config, region_index, offset, bytes);
        }

        let capacity_bytes = config.region_bytes.max(bytes);
        self.regions.push(OldRegion {
            capacity_bytes,
            used_bytes: 0,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
        let region_index = self.regions.len() - 1;
        let offset = self.regions[region_index].used_bytes;
        self.regions[region_index].used_bytes = bytes;
        self.make_placement(config, region_index, offset, bytes)
    }

    pub(crate) fn record_object(&mut self, object: &ObjectRecord) {
        let Some(placement) = object.old_region_placement() else {
            return;
        };
        let region = &mut self.regions[placement.region_index];
        region.live_bytes = region.live_bytes.saturating_add(object.total_size());
        region.object_count = region.object_count.saturating_add(1);
        for line in placement.line_start..placement.line_start + placement.line_count {
            region.occupied_lines.insert(line);
        }
    }

    pub(crate) fn region_stats(&self) -> Vec<OldRegionStats> {
        self.regions
            .iter()
            .enumerate()
            .map(|(region_index, region)| OldRegionStats {
                region_index,
                reserved_bytes: region.capacity_bytes,
                used_bytes: region.used_bytes,
                live_bytes: region.live_bytes,
                free_bytes: region.capacity_bytes.saturating_sub(region.live_bytes),
                hole_bytes: region.used_bytes.saturating_sub(region.live_bytes),
                tail_bytes: region.capacity_bytes.saturating_sub(region.used_bytes),
                object_count: region.object_count,
                occupied_lines: region.occupied_lines.len(),
            })
            .collect()
    }

    pub(crate) fn prepare_rebuild(
        &mut self,
        completed_plan: Option<&CollectionPlan>,
    ) -> Option<OldRegionRebuildState> {
        if !completed_plan
            .is_some_and(|plan| matches!(plan.kind, CollectionKind::Major | CollectionKind::Full))
        {
            return None;
        }
        let previous_regions = core::mem::take(&mut self.regions);
        prepare_old_region_rebuild_for_plan(&previous_regions, completed_plan)
    }

    pub(crate) fn prepare_rebuild_for_plan(&self, plan: &CollectionPlan) -> OldRegionRebuildState {
        prepare_old_region_rebuild_for_plan(&self.regions, Some(plan))
            .expect("major reclaim preparation requires a major/full plan")
    }

    pub(crate) fn rebuild_post_sweep_object(
        config: &OldGenConfig,
        object: &mut ObjectRecord,
        total_size: usize,
        rebuild: Option<&mut OldRegionRebuildState>,
    ) {
        let Some(rebuild) = rebuild else {
            return;
        };
        let Some(mut placement) = object.old_region_placement() else {
            return;
        };
        if rebuild.selected_regions.contains(&placement.region_index) {
            let compacted =
                Self::reserve_rebuild_placement(&mut rebuild.compacted_regions, config, total_size);
            placement.region_index = rebuild.compacted_base_index + compacted.region_index;
            placement.offset_bytes = compacted.offset_bytes;
            placement.line_start = compacted.line_start;
            placement.line_count = compacted.line_count;
            object.set_old_region_placement(placement);
            let region = &mut rebuild.compacted_regions[compacted.region_index];
            region.live_bytes = region.live_bytes.saturating_add(total_size);
            region.object_count = region.object_count.saturating_add(1);
            for line in placement.line_start..placement.line_start + placement.line_count {
                region.occupied_lines.insert(line);
            }
            return;
        }

        let Some(&new_index) = rebuild.preserved_index_map.get(&placement.region_index) else {
            return;
        };
        if placement.region_index != new_index {
            placement.region_index = new_index;
            object.set_old_region_placement(placement);
        }
        let region = &mut rebuild.rebuilt_regions[new_index];
        region.live_bytes = region.live_bytes.saturating_add(total_size);
        region.object_count = region.object_count.saturating_add(1);
        for line in placement.line_start..placement.line_start + placement.line_count {
            region.occupied_lines.insert(line);
        }
    }

    pub(crate) fn finish_rebuild(
        rebuild: Option<OldRegionRebuildState>,
        objects: &mut [ObjectRecord],
    ) -> (Option<Vec<OldRegion>>, OldRegionCollectionStats) {
        let Some(rebuild) = rebuild else {
            return (None, OldRegionCollectionStats::default());
        };
        let provisional_compacted_base = rebuild.compacted_base_index;
        let mut preserved_index_remap = vec![None; provisional_compacted_base];
        let mut compacted_regions = Vec::with_capacity(
            rebuild
                .rebuilt_regions
                .len()
                .saturating_add(rebuild.compacted_regions.len()),
        );
        for (old_index, region) in rebuild.rebuilt_regions.into_iter().enumerate() {
            if region.object_count == 0 {
                continue;
            }
            preserved_index_remap[old_index] = Some(compacted_regions.len());
            compacted_regions.push(region);
        }
        let new_compacted_base = compacted_regions.len();
        compacted_regions.extend(rebuild.compacted_regions);
        for object in objects.iter_mut() {
            if object.space() != SpaceKind::Old {
                continue;
            }
            let Some(mut placement) = object.old_region_placement() else {
                continue;
            };
            if placement.region_index < provisional_compacted_base {
                let Some(new_index) = preserved_index_remap[placement.region_index] else {
                    continue;
                };
                if placement.region_index != new_index {
                    placement.region_index = new_index;
                    object.set_old_region_placement(placement);
                }
                continue;
            }

            let compacted_offset = placement
                .region_index
                .saturating_sub(provisional_compacted_base);
            let new_index = new_compacted_base.saturating_add(compacted_offset);
            if placement.region_index != new_index {
                placement.region_index = new_index;
                object.set_old_region_placement(placement);
            }
        }
        let reclaimed_regions = rebuild
            .previous_region_count
            .saturating_sub(compacted_regions.len()) as u64;
        (
            Some(compacted_regions),
            OldRegionCollectionStats {
                compacted_regions: rebuild.compacted_regions_count,
                reclaimed_regions,
            },
        )
    }

    pub(crate) fn finish_prepared_rebuild(
        rebuild: OldRegionRebuildState,
        survivors: &mut [PreparedReclaimSurvivor],
    ) -> (Vec<OldRegion>, OldRegionCollectionStats) {
        let provisional_compacted_base = rebuild.compacted_base_index;
        let mut preserved_index_remap = vec![None; provisional_compacted_base];
        let mut compacted_regions = Vec::with_capacity(
            rebuild
                .rebuilt_regions
                .len()
                .saturating_add(rebuild.compacted_regions.len()),
        );
        for (old_index, region) in rebuild.rebuilt_regions.into_iter().enumerate() {
            if region.object_count == 0 {
                continue;
            }
            preserved_index_remap[old_index] = Some(compacted_regions.len());
            compacted_regions.push(region);
        }
        let new_compacted_base = compacted_regions.len();
        compacted_regions.extend(rebuild.compacted_regions);
        for survivor in survivors.iter_mut() {
            let Some(placement) = survivor.old_region_placement.as_mut() else {
                continue;
            };
            if placement.region_index < provisional_compacted_base {
                let Some(new_index) = preserved_index_remap[placement.region_index] else {
                    continue;
                };
                placement.region_index = new_index;
            } else {
                placement.region_index = placement
                    .region_index
                    .saturating_sub(provisional_compacted_base)
                    .saturating_add(new_compacted_base);
            }
        }
        let reclaimed_regions = rebuild
            .previous_region_count
            .saturating_sub(compacted_regions.len()) as u64;
        (
            compacted_regions,
            OldRegionCollectionStats {
                compacted_regions: rebuild.compacted_regions_count,
                reclaimed_regions,
            },
        )
    }

    pub(crate) fn reserve_rebuild_placement(
        regions: &mut Vec<OldRegion>,
        config: &OldGenConfig,
        bytes: usize,
    ) -> OldRegionPlacement {
        let align = config.line_bytes.max(8);

        for (region_index, region) in regions.iter_mut().enumerate() {
            let offset = align_up(region.used_bytes, align);
            if offset.saturating_add(bytes) <= region.capacity_bytes {
                region.used_bytes = offset.saturating_add(bytes);
                return Self::make_placement_for_config(config, region_index, offset, bytes);
            }
        }

        let capacity_bytes = config.region_bytes.max(bytes);
        regions.push(OldRegion {
            capacity_bytes,
            used_bytes: bytes,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
        let region_index = regions.len() - 1;
        Self::make_placement_for_config(config, region_index, 0, bytes)
    }

    fn try_reserve_in_existing_region(
        &mut self,
        bytes: usize,
        align: usize,
    ) -> Option<(usize, usize)> {
        for (region_index, region) in self.regions.iter_mut().enumerate() {
            let offset = align_up(region.used_bytes, align);
            if offset.saturating_add(bytes) <= region.capacity_bytes {
                region.used_bytes = offset.saturating_add(bytes);
                return Some((region_index, offset));
            }
        }
        None
    }

    fn make_placement(
        &self,
        config: &OldGenConfig,
        region_index: usize,
        offset_bytes: usize,
        bytes: usize,
    ) -> OldRegionPlacement {
        Self::make_placement_for_config(config, region_index, offset_bytes, bytes)
    }

    fn make_placement_for_config(
        config: &OldGenConfig,
        region_index: usize,
        offset_bytes: usize,
        bytes: usize,
    ) -> OldRegionPlacement {
        let line_bytes = config.line_bytes.max(1);
        let line_start = offset_bytes / line_bytes;
        let line_count = bytes.div_ceil(line_bytes).max(1);
        OldRegionPlacement {
            region_index,
            offset_bytes,
            line_start,
            line_count,
        }
    }
}

#[derive(Debug)]
pub(crate) struct OldRegion {
    pub(crate) capacity_bytes: usize,
    pub(crate) used_bytes: usize,
    pub(crate) live_bytes: usize,
    pub(crate) object_count: usize,
    pub(crate) occupied_lines: HashSet<usize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OldRegionCollectionStats {
    pub(crate) compacted_regions: u64,
    pub(crate) reclaimed_regions: u64,
}

#[derive(Debug)]
pub(crate) struct OldRegionRebuildState {
    pub(crate) previous_region_count: usize,
    pub(crate) preserved_index_map: HashMap<usize, usize>,
    pub(crate) selected_regions: HashSet<usize>,
    pub(crate) compacted_base_index: usize,
    pub(crate) compacted_regions_count: u64,
    pub(crate) rebuilt_regions: Vec<OldRegion>,
    pub(crate) compacted_regions: Vec<OldRegion>,
}

pub(crate) fn prepare_old_region_rebuild_for_plan(
    previous_regions: &[OldRegion],
    completed_plan: Option<&CollectionPlan>,
) -> Option<OldRegionRebuildState> {
    let plan = completed_plan
        .filter(|plan| matches!(plan.kind, CollectionKind::Major | CollectionKind::Full))?;
    let previous_region_count = previous_regions.len();
    let selected_regions: HashSet<_> = plan.selected_old_regions.iter().copied().collect();
    let mut rebuilt_regions = Vec::new();
    let mut preserved_index_map = HashMap::new();
    for (old_index, region) in previous_regions.iter().enumerate() {
        if selected_regions.contains(&old_index) {
            continue;
        }
        preserved_index_map.insert(old_index, rebuilt_regions.len());
        rebuilt_regions.push(OldRegion {
            capacity_bytes: region.capacity_bytes,
            used_bytes: region.used_bytes,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
    }
    let compacted_base_index = rebuilt_regions.len();
    Some(OldRegionRebuildState {
        previous_region_count,
        preserved_index_map,
        selected_regions,
        compacted_base_index,
        compacted_regions_count: plan.selected_old_regions.len() as u64,
        rebuilt_regions,
        compacted_regions: Vec::new(),
    })
}

pub(crate) fn compare_compaction_candidate_priority(
    left: &OldRegionStats,
    right: &OldRegionStats,
) -> core::cmp::Ordering {
    let left_live = left.live_bytes.max(1) as u128;
    let right_live = right.live_bytes.max(1) as u128;
    let left_efficiency = (left.hole_bytes as u128).saturating_mul(right_live);
    let right_efficiency = (right.hole_bytes as u128).saturating_mul(left_live);

    right_efficiency
        .cmp(&left_efficiency)
        .then_with(|| right.hole_bytes.cmp(&left.hole_bytes))
        .then_with(|| left.live_bytes.cmp(&right.live_bytes))
        .then_with(|| right.free_bytes.cmp(&left.free_bytes))
        .then_with(|| left.object_count.cmp(&right.object_count))
        .then_with(|| left.region_index.cmp(&right.region_index))
}

fn align_up(value: usize, align: usize) -> usize {
    if align <= 1 {
        value
    } else {
        let rem = value % align;
        if rem == 0 {
            value
        } else {
            value + (align - rem)
        }
    }
}
