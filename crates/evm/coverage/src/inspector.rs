use crate::{CallData, HitMap, HitMaps};
use alloy_primitives::{Address, B256, Bytes, address, map::B256HashMap};
use revm::{
    Inspector,
    context::{ContextTr, JournalTr},
    interpreter::{
        CallInputs, CallOutcome, CallScheme, CreateInputs, CreateOutcome, Gas, InstructionResult,
        Interpreter, InterpreterResult, interpreter_types::Jumps,
    },
};
use std::ptr::NonNull;

/// Address used by source instrumentation to report coverage hits.
pub const FOUNDRY_COVERAGE_ADDRESS: Address = address!("c0bEc0BEc0BeC0bEC0beC0bEC0bEC0beC0beC0BE");

/// Inspector implementation for collecting coverage information.
#[derive(Clone, Debug)]
pub struct LineCoverageCollector {
    // NOTE: `current_map` is always a valid reference into `maps`.
    // It is accessed only through `get_or_insert_map` which guarantees that it's valid.
    // Both of these fields are unsafe to access directly outside of `*insert_map`.
    current_map: NonNull<HitMap>,
    current_hash: B256,

    maps: HitMaps,
}

// SAFETY: See comments on `current_map`.
unsafe impl Send for LineCoverageCollector {}
unsafe impl Sync for LineCoverageCollector {}

impl Default for LineCoverageCollector {
    fn default() -> Self {
        Self {
            current_map: NonNull::dangling(),
            current_hash: B256::ZERO,
            maps: Default::default(),
        }
    }
}

impl<CTX> Inspector<CTX> for LineCoverageCollector {
    fn initialize_interp(&mut self, interpreter: &mut Interpreter, _context: &mut CTX) {
        let call = interpreter.input.bytecode_address.is_some().then(|| {
            let calldata = if interpreter.input.input.is_empty() {
                CallData::Empty
            } else {
                let input = interpreter.input.input.as_bytes_memory(&interpreter.memory);
                CallData::new(&input)
            };
            (calldata, !interpreter.input.call_value.is_zero())
        });
        let map = self.get_or_insert_map(interpreter);
        if let Some((call, with_value)) = call {
            map.call(call, with_value);
        }
        // Reserve some space early to avoid reallocating too often.
        map.reserve(8192.min(interpreter.bytecode.len()));
    }

    fn step(&mut self, interpreter: &mut Interpreter, _context: &mut CTX) {
        let map = self.get_or_insert_map(interpreter);
        map.hit(interpreter.bytecode.pc() as u32);
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        if outcome.result.result.is_ok()
            && let Some(map) = self.maps.get_mut(&inputs.init_code_hash())
        {
            map.creation();
        }
    }
}

impl LineCoverageCollector {
    /// Finish collecting coverage information and return the [`HitMaps`].
    pub fn finish(self) -> HitMaps {
        self.maps
    }

    /// Gets the hit map for the current contract, or inserts a new one if it doesn't exist.
    ///
    /// The map is stored in `current_map` and returned as a mutable reference.
    /// See comments on `current_map` for more details.
    #[inline]
    fn get_or_insert_map(&mut self, interpreter: &mut Interpreter) -> &mut HitMap {
        let hash = interpreter.bytecode.get_or_calculate_hash();
        if self.current_hash != *hash {
            self.insert_map(interpreter);
        }
        // SAFETY: See comments on `current_map`.
        unsafe { self.current_map.as_mut() }
    }

    #[cold]
    #[inline(never)]
    fn insert_map(&mut self, interpreter: &mut Interpreter) {
        let hash = interpreter.bytecode.hash().unwrap();
        self.current_hash = hash;
        // Converts the mutable reference to a `NonNull` pointer.
        self.current_map = self
            .maps
            .entry(hash)
            .or_insert_with(|| HitMap::new(interpreter.bytecode.original_bytes()))
            .into();
    }
}

/// Hit counts keyed by instrumented coverage tag.
#[derive(Clone, Debug, Default)]
pub struct InstrumentedHitMaps(pub B256HashMap<u64>);

impl InstrumentedHitMaps {
    /// Merge `other` into `target`.
    pub fn merge_opt(target: &mut Option<Self>, other: Option<Self>) {
        let Some(other) = other else { return };
        if let Some(target) = target {
            target.merge(other);
        } else {
            *target = Some(other);
        }
    }

    /// Merge `other` into this map.
    pub fn merge(&mut self, other: Self) {
        for (tag, hits) in other.0 {
            *self.0.entry(tag).or_default() += hits;
        }
    }

    /// Merge borrowed hits into this map.
    pub fn merge_ref(&mut self, other: &Self) {
        for (tag, hits) in &other.0 {
            *self.0.entry(*tag).or_default() += *hits;
        }
    }

    fn hit(&mut self, tag: B256) {
        *self.0.entry(tag).or_default() += 1;
    }
}

/// Inspector implementation for source-instrumented coverage calls.
#[derive(Clone, Debug, Default)]
pub struct InstrumentedCoverageCollector {
    hits: InstrumentedHitMaps,
    frame_return_data: Vec<Bytes>,
}

impl InstrumentedCoverageCollector {
    /// Finish collecting instrumented coverage information.
    pub fn finish(self) -> InstrumentedHitMaps {
        self.hits
    }

    /// Returns `true` if `inputs` is a coverage probe call: a `STATICCALL` to the sentinel
    /// address. Restricting to `STATICCALL` (which cannot transfer value) avoids hijacking an
    /// unrelated `CALL`/`DELEGATECALL` that happens to target the sentinel address.
    fn is_coverage_call(inputs: &CallInputs) -> bool {
        inputs.bytecode_address == FOUNDRY_COVERAGE_ADDRESS
            && inputs.scheme == CallScheme::StaticCall
    }

    fn frame_depth<CTX: ContextTr>(context: &CTX) -> usize {
        context.journal().depth()
    }

    fn frame_return_data(&self, depth: usize) -> Bytes {
        self.frame_return_data.get(depth).cloned().unwrap_or_default()
    }

    fn clear_frame_return_data(&mut self, depth: usize) {
        if self.frame_return_data.len() <= depth {
            self.frame_return_data.resize_with(depth + 1, Bytes::new);
        }
        self.frame_return_data[depth].clear();
    }

    fn set_frame_return_data(&mut self, depth: usize, output: Bytes) {
        if self.frame_return_data.len() <= depth {
            self.frame_return_data.resize_with(depth + 1, Bytes::new);
        }
        self.frame_return_data[depth] = output;
    }
}

impl<CTX: ContextTr> Inspector<CTX> for InstrumentedCoverageCollector {
    fn initialize_interp(&mut self, _interpreter: &mut Interpreter, context: &mut CTX) {
        self.clear_frame_return_data(Self::frame_depth(context));
    }

    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        if !Self::is_coverage_call(inputs) {
            return None;
        }

        // The probe forwards exactly the 32-byte tag as calldata; anything else is not ours.
        let input = inputs.input.bytes(context);
        if input.len() == 32 {
            self.hits.hit(B256::from_slice(&input[..32]));
        }

        Some(CallOutcome {
            result: InterpreterResult {
                result: InstructionResult::Return,
                output: self.frame_return_data(Self::frame_depth(context)),
                gas: Gas::new(inputs.gas_limit),
            },
            memory_offset: inputs.return_memory_offset.clone(),
            was_precompile_called: true,
            precompile_call_logs: vec![],
            charged_new_account_state_gas: false,
        })
    }

    fn call_end(&mut self, _context: &mut CTX, inputs: &CallInputs, outcome: &mut CallOutcome) {
        if !Self::is_coverage_call(inputs) {
            self.set_frame_return_data(Self::frame_depth(_context), outcome.result.output.clone());
        }
    }

    fn create_end(
        &mut self,
        _context: &mut CTX,
        _inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        let output = if outcome.result.result == InstructionResult::Revert {
            outcome.result.output.clone()
        } else {
            Bytes::new()
        };
        self.set_frame_return_data(Self::frame_depth(_context), output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_instrumented_merge_retains_allocation() {
        let mut incoming = InstrumentedHitMaps::default();
        incoming.hit(B256::ZERO);
        let allocation = incoming.0.get(&B256::ZERO).unwrap() as *const u64;
        let mut target = None;
        InstrumentedHitMaps::merge_opt(&mut target, Some(incoming));
        let merged = target.unwrap();
        assert_eq!(merged.0.get(&B256::ZERO), Some(&1));
        assert_eq!(merged.0.get(&B256::ZERO).unwrap() as *const u64, allocation);
    }

    #[test]
    fn instrumented_merge_sums_overlapping_tags() {
        let mut target = InstrumentedHitMaps::default();
        target.hit(B256::ZERO);
        let mut incoming = InstrumentedHitMaps::default();
        incoming.hit(B256::ZERO);
        incoming.hit(B256::with_last_byte(1));
        let mut target = Some(target);
        InstrumentedHitMaps::merge_opt(&mut target, Some(incoming));
        InstrumentedHitMaps::merge_opt(&mut target, None);
        let merged = target.unwrap();
        assert_eq!(merged.0.get(&B256::ZERO), Some(&2));
        assert_eq!(merged.0.get(&B256::with_last_byte(1)), Some(&1));
    }
}
