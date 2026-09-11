use super::{call_after_invariant_function, call_invariant_function, execute_tx};
use crate::executors::{
    EarlyExit, Executor,
    invariant::shrink::{
        CheckSequenceOutcome, ShrinkProgress, shrink_sequence, shrink_sequence_value,
    },
};
use alloy_dyn_abi::JsonAbiExt;
use alloy_json_abi::Function;
use alloy_primitives::{
    Bytes, I256, Log,
    map::{AddressHashMap, HashMap},
};
use eyre::Result;
use foundry_common::{ContractsByAddress, ContractsByArtifact};
use foundry_config::InvariantConfig;
use foundry_evm_core::{decode::RevertDecoder, evm::FoundryEvmNetwork};
use foundry_evm_coverage::{HitMaps, InstrumentedHitMaps};
use foundry_evm_fuzz::{BaseCounterExample, BasicTxDetails, invariant::InvariantContract};
use foundry_evm_traces::{TraceKind, TraceRequirements, Traces, load_contracts};
use indicatif::ProgressBar;
use parking_lot::RwLock;
use std::sync::Arc;

pub struct ReplayErrorResult {
    pub counterexample_sequence: Vec<BaseCounterExample>,
    pub check_result: Option<CheckSequenceOutcome>,
    pub fork_block_number: Option<u64>,
}

/// Replays a call sequence for collecting logs and traces.
/// Returns counterexample to be used when the call sequence is a failed scenario.
#[expect(clippy::too_many_arguments)]
pub fn replay_run<FEN: FoundryEvmNetwork>(
    invariant_contract: &InvariantContract<'_>,
    target_invariant: &Function,
    mut executor: Executor<FEN>,
    known_contracts: &ContractsByArtifact,
    mut ided_contracts: ContractsByAddress,
    logs: &mut Vec<Log>,
    traces: &mut Traces,
    debug_bytecodes: &mut AddressHashMap<Bytes>,
    line_coverage: &mut Option<HitMaps>,
    instrumented_coverage: &mut Option<InstrumentedHitMaps>,
    deprecated_cheatcodes: &mut HashMap<&'static str, Option<&'static str>>,
    inputs: &[BasicTxDetails],
    show_solidity: bool,
) -> Result<ReplayErrorResult> {
    // We want traces for a failed case.
    if executor.inspector().tracer.is_none() {
        executor.set_trace_requirements(TraceRequirements::none().with_calls(true));
    }

    let mut counterexample_sequence = vec![];

    // Replay each call from the sequence, collect logs, traces and coverage.
    for tx in inputs {
        let mut call_result = execute_tx(&mut executor, tx)?;
        logs.append(&mut call_result.logs);
        debug_bytecodes.extend(std::mem::take(&mut call_result.debug_bytecodes));
        traces.push((TraceKind::Execution, call_result.traces.clone().unwrap()));
        HitMaps::merge_opt(line_coverage, call_result.line_coverage.take());
        InstrumentedHitMaps::merge_opt(
            instrumented_coverage,
            call_result.instrumented_coverage.take(),
        );

        // Commit state changes to persist across calls in the sequence.
        executor.commit(&mut call_result);

        // Identify newly generated contracts, if they exist.
        ided_contracts
            .extend(load_contracts(call_result.traces.iter().map(|a| &a.arena), known_contracts));

        // Create counter example to be used in failed case.
        counterexample_sequence.push(BaseCounterExample::from_invariant_call(
            tx,
            &ided_contracts,
            call_result.traces,
            show_solidity,
        ));
    }

    // Replay invariant to collect logs and traces.
    // We do this only once at the end of the replayed sequence.
    // Checking after each call doesn't add valuable info for passing scenario
    // (invariant call result is always success) nor for failed scenarios
    // (invariant call result is always success until the last call that breaks it).
    let (invariant_result, invariant_success) = call_invariant_function(
        &executor,
        invariant_contract.address,
        target_invariant.abi_encode_input(&[])?.into(),
    )?;
    let fork_block_number = invariant_result.fork_block_number;
    debug_bytecodes.extend(invariant_result.debug_bytecodes);
    traces.push((TraceKind::Execution, invariant_result.traces.clone().unwrap()));
    logs.extend(invariant_result.logs);
    deprecated_cheatcodes.extend(
        invariant_result
            .cheatcodes
            .as_ref()
            .map_or_else(Default::default, |cheats| cheats.deprecated.clone()),
    );

    // Collect after invariant logs and traces.
    if invariant_contract.call_after_invariant && invariant_success {
        let (after_invariant_result, _) =
            call_after_invariant_function(&executor, invariant_contract.address)?;
        debug_bytecodes.extend(after_invariant_result.debug_bytecodes);
        traces.push((TraceKind::Execution, after_invariant_result.traces.clone().unwrap()));
        logs.extend(after_invariant_result.logs);
    }

    Ok(ReplayErrorResult { counterexample_sequence, check_result: None, fork_block_number })
}

/// Replays and shrinks a call sequence, collecting logs and traces.
///
/// For check mode (target_value=None): shrinks to find shortest failing sequence.
/// For optimization mode (target_value=Some): shrinks to find shortest sequence producing target.
#[expect(clippy::too_many_arguments)]
pub fn replay_error<FEN: FoundryEvmNetwork>(
    config: InvariantConfig,
    mut executor: Executor<FEN>,
    calls: &[BasicTxDetails],
    inner_sequence: Option<Vec<Option<BasicTxDetails>>>,
    expect_assertion_failure: bool,
    rd: Option<&RevertDecoder>,
    target_value: Option<I256>,
    invariant_contract: &InvariantContract<'_>,
    target_invariant: &Function,
    known_contracts: &ContractsByArtifact,
    ided_contracts: ContractsByAddress,
    logs: &mut Vec<Log>,
    traces: &mut Traces,
    debug_bytecodes: &mut AddressHashMap<Bytes>,
    line_coverage: &mut Option<HitMaps>,
    instrumented_coverage: &mut Option<InstrumentedHitMaps>,
    deprecated_cheatcodes: &mut HashMap<&'static str, Option<&'static str>>,
    progress: Option<&ProgressBar>,
    early_exit: &EarlyExit,
    position: Option<(usize, usize)>,
) -> Result<ReplayErrorResult> {
    // Multi-invariant runs include `[i/N]` in the shrink progress message so users see how many
    // shrinkers are queued behind the current one.
    let shrink_progress = ShrinkProgress::new(
        &config,
        progress,
        &target_invariant.name,
        position,
        Some(&ided_contracts),
        config.show_solidity,
    );

    let (calls, check_result) = if let Some(target) = target_value {
        (
            shrink_sequence_value(
                &config,
                invariant_contract,
                target_invariant,
                calls,
                &executor,
                target,
                &shrink_progress,
                early_exit,
            )?,
            None,
        )
    } else {
        let shrunk = shrink_sequence(
            &config,
            invariant_contract,
            target_invariant,
            calls,
            expect_assertion_failure,
            &executor,
            rd,
            &shrink_progress,
            early_exit,
        )?;
        (shrunk.calls, shrunk.result)
    };

    if let Some(sequence) = inner_sequence {
        set_up_inner_replay(&mut executor, &sequence);
    }

    let mut replay = replay_run(
        invariant_contract,
        target_invariant,
        executor,
        known_contracts,
        ided_contracts,
        logs,
        traces,
        debug_bytecodes,
        line_coverage,
        instrumented_coverage,
        deprecated_cheatcodes,
        &calls,
        config.show_solidity,
    )?;

    replay.check_result = check_result;
    Ok(replay)
}

/// Sets up the calls generated by the internal fuzzer, if they exist.
fn set_up_inner_replay<FEN: FoundryEvmNetwork>(
    executor: &mut Executor<FEN>,
    inner_sequence: &[Option<BasicTxDetails>],
) {
    if let Some(fuzzer) = &mut executor.inspector_mut().fuzzer
        && let Some(call_generator) = &mut fuzzer.call_generator
    {
        call_generator.last_sequence = Arc::new(RwLock::new(inner_sequence.to_owned()));
        call_generator.set_replay(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executors::ExecutorBuilder;
    use alloy_json_abi::JsonAbi;
    use alloy_primitives::{Address, B256};
    use foundry_evm_core::{EvmEnv, backend::Backend, evm::EthEvmNetwork};
    use foundry_evm_coverage::FOUNDRY_COVERAGE_ADDRESS;
    use foundry_evm_fuzz::CallDetails;
    use revm::{bytecode::Bytecode, context::TxEnv};

    #[test]
    fn replayed_sequence_retains_instrumented_hits() {
        let mut executor = ExecutorBuilder::<EthEvmNetwork>::default()
            .inspectors(|stack| stack.instrumented_coverage(true))
            .gas_limit(1_000_000)
            .build(
                EvmEnv::default(),
                TxEnv::default(),
                Backend::spawn(None).unwrap(),
                Default::default(),
            );
        executor.evm_env_mut().cfg_env.disable_nonce_check = true;
        let target = Address::repeat_byte(0x11);
        let invariant_address = Address::repeat_byte(0x22);
        let mut code = vec![0x60, 0x01, 0x5f, 0x52, 0x5f, 0x5f, 0x60, 0x20, 0x5f, 0x73];
        code.extend_from_slice(FOUNDRY_COVERAGE_ADDRESS.as_slice());
        code.extend_from_slice(&[0x5a, 0xfa, 0x50, 0x00]);
        executor.set_code(target, Bytecode::new_raw(code.into())).unwrap();
        executor
            .set_code(invariant_address, Bytecode::new_raw(Bytes::from_static(&[0x00])))
            .unwrap();
        let function = Function::parse("invariant_ok()").unwrap();
        let abi = JsonAbi::new();
        let contract = InvariantContract::new(
            invariant_address,
            "InvariantTest",
            vec![(&function, false)],
            0,
            false,
            &abi,
        );
        let tx = BasicTxDetails {
            warp: None,
            roll: None,
            sender: Address::ZERO,
            call_details: CallDetails { target, calldata: Bytes::new(), value: None },
        };
        let mut hits = None;
        replay_run(
            &contract,
            &function,
            executor,
            &Default::default(),
            Default::default(),
            &mut vec![],
            &mut vec![],
            &mut Default::default(),
            &mut None,
            &mut hits,
            &mut Default::default(),
            &[tx.clone(), tx],
            false,
        )
        .unwrap();
        let hits = hits.expect("invariant replay lost its coverage");
        assert_eq!(hits.0.len(), 1);
        assert_eq!(hits.0.get(&B256::with_last_byte(1)), Some(&2));
    }
}
