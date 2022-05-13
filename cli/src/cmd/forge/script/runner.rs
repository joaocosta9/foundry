use ethers::types::{Address, Bytes, NameOrAddress, U256};
use forge::{
    executor::{CallResult, DatabaseRef, DeployResult, EvmError, Executor, RawCallResult},
    trace::{CallTraceArena, TraceKind},
    CALLER,
};

use super::*;
pub struct Runner<DB: DatabaseRef> {
    pub executor: Executor<DB>,
    pub initial_balance: U256,
    pub sender: Address,
}

impl<DB: DatabaseRef> Runner<DB> {
    pub fn new(executor: Executor<DB>, initial_balance: U256, sender: Address) -> Self {
        Self { executor, initial_balance, sender }
    }

    /// Deploys the libraries and broadcast contract. Calls setUp method if requested.
    pub fn setup(
        &mut self,
        libraries: &[Bytes],
        code: Bytes,
        setup: bool,
        is_broadcast: bool,
    ) -> eyre::Result<(Address, ScriptResult)> {
        if !is_broadcast {
            // We max out their balance so that they can deploy and make calls.
            self.executor.set_balance(self.sender, U256::MAX);

            // We set the nonce of the deployer accounts to 1 to get the same addresses as DappTools
            self.executor.set_nonce(self.sender, 1);
        }

        // We max out their balance so that they can deploy and make calls.
        self.executor.set_balance(*CALLER, U256::MAX);

        // Deploy libraries
        let mut traces: Vec<(TraceKind, CallTraceArena)> = libraries
            .iter()
            .filter_map(|code| {
                let DeployResult { traces, .. } = self
                    .executor
                    .deploy(self.sender, code.0.clone(), 0u32.into(), None)
                    .expect("couldn't deploy library");

                traces
            })
            .map(|traces| (TraceKind::Deployment, traces))
            .collect();

        // Deploy an instance of the contract
        let DeployResult {
            address,
            mut logs,
            traces: constructor_traces,
            debug: constructor_debug,
            ..
        } = self.executor.deploy(*CALLER, code.0, 0u32.into(), None).expect("couldn't deploy");
        traces.extend(constructor_traces.map(|traces| (TraceKind::Deployment, traces)).into_iter());
        self.executor.set_balance(address, self.initial_balance);

        // Optionally call the `setUp` function
        let (success, gas, labeled_addresses, transactions, debug) = if !setup {
            (true, 0, Default::default(), None, vec![constructor_debug].into_iter().collect())
        } else {
            match self.executor.setup(address) {
                Ok(CallResult {
                    reverted,
                    traces: setup_traces,
                    labels,
                    logs: setup_logs,
                    debug,
                    gas,
                    transactions,
                    ..
                }) |
                Err(EvmError::Execution {
                    reverted,
                    traces: setup_traces,
                    labels,
                    logs: setup_logs,
                    debug,
                    gas,
                    transactions,
                    ..
                }) => {
                    traces
                        .extend(setup_traces.map(|traces| (TraceKind::Setup, traces)).into_iter());
                    logs.extend_from_slice(&setup_logs);

                    (
                        !reverted,
                        gas,
                        labels,
                        transactions,
                        vec![constructor_debug, debug].into_iter().collect(),
                    )
                }
                Err(e) => return Err(e.into()),
            }
        };

        Ok((
            address,
            ScriptResult {
                returned: bytes::Bytes::new(),
                success,
                gas,
                labeled_addresses,
                transactions,
                logs,
                traces,
                debug,
            },
        ))
    }

    /// Executes the method that will collect all broadcastable transactions.
    pub fn script(&mut self, address: Address, calldata: Bytes) -> eyre::Result<ScriptResult> {
        self.call(*CALLER, address, calldata, U256::zero(), false)
    }

    /// Runs a broadcastable transaction locally and persists its state.
    pub fn sim(
        &mut self,
        from: Address,
        to: Option<NameOrAddress>,
        calldata: Option<Bytes>,
        value: Option<U256>,
    ) -> eyre::Result<ScriptResult> {
        if let Some(NameOrAddress::Address(to)) = to {
            self.call(from, to, calldata.unwrap_or_default(), value.unwrap_or(U256::zero()), true)
        } else if to.is_none() {
            let DeployResult { address: _, gas, logs, traces, debug } = self.executor.deploy(
                from,
                calldata.expect("No data for create transaction").0,
                value.unwrap_or(U256::zero()),
                None,
            )?;

            Ok(ScriptResult {
                returned: bytes::Bytes::new(),
                success: true,
                gas,
                logs,
                traces: traces
                    .map(|mut traces| {
                        // Manually adjust gas for the trace to add back the stipend/real used gas
                        traces.arena[0].trace.gas_cost = gas;
                        vec![(TraceKind::Execution, traces)]
                    })
                    .unwrap_or_default(),
                debug: vec![debug].into_iter().collect(),
                labeled_addresses: Default::default(),
                transactions: Default::default(),
            })
        } else {
            panic!("ens not supported");
        }
    }

    fn call(
        &mut self,
        from: Address,
        to: Address,
        calldata: Bytes,
        value: U256,
        commit: bool,
    ) -> eyre::Result<ScriptResult> {
        let RawCallResult {
            result,
            reverted,
            gas,
            stipend,
            logs,
            traces,
            labels,
            debug,
            transactions,
            ..
        } = if !commit {
            self.executor.call_raw(from, to, calldata.0, value)?
        } else {
            self.executor.call_raw_committing(from, to, calldata.0, value)?
        };

        Ok(ScriptResult {
            returned: result,
            success: !reverted,
            gas: gas.overflowing_sub(stipend).0,
            logs,
            traces: traces
                .map(|mut traces| {
                    // Manually adjust gas for the trace to add back the stipend/real used gas
                    traces.arena[0].trace.gas_cost = gas;
                    vec![(TraceKind::Execution, traces)]
                })
                .unwrap_or_default(),
            debug: vec![debug].into_iter().collect(),
            labeled_addresses: labels,
            transactions,
        })
    }
}
