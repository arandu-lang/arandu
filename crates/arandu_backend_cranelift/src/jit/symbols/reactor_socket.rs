use arandu_middle::Diagnostic;
use cranelift_codegen::ir::types::{I32, I64};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_codegen::isa::CallConv;
use cranelift_module::Linkage;
use rustc_hash::FxHashMap;

use super::codegen_ice;
use super::insert_sym;

pub(super) fn declare_reactor_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // SL_R.2 reactor host imports
    {
        let mut create_sig = Signature::new(default_call_conv);
        create_sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_create", Linkage::Import, &create_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_reactor_create: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_create", id);

        let mut destroy_sig = Signature::new(default_call_conv);
        destroy_sig.params.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_destroy", Linkage::Import, &destroy_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_reactor_destroy: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_destroy", id);

        let mut two_i64_ret_i64 = Signature::new(default_call_conv);
        two_i64_ret_i64.params.push(AbiParam::new(I64));
        two_i64_ret_i64.params.push(AbiParam::new(I64));
        two_i64_ret_i64.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_reactor_sleep_ms",
            "ar_rt_reactor_arm_timer_ms",
            "ar_rt_reactor_poll_ms",
            "ar_rt_waker_wait",
            "ar_rt_supervisor_poll",
            "ar_rt_supervisor_wait",
            "ar_rt_supervisor_kill",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &two_i64_ret_i64)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut sig = Signature::new(default_call_conv);
        for _ in 0..4 {
            sig.params.push(AbiParam::new(I64));
        }
        sig.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_reactor_register_socket", Linkage::Import, &sig)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_reactor_register_socket: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_reactor_register_socket", id);

        let mut zero_ret = Signature::new(default_call_conv);
        zero_ret.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_reactor_backend",
            "ar_rt_waker_create",
            "ar_rt_supervisor_create",
        ] {
            if !func_ids.contains_key(name) {
                let id = module
                    .declare_function(name, Linkage::Import, &zero_ret)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                insert_sym(func_ids, name, id);
            }
        }

        let mut void_sig = Signature::new(default_call_conv);
        void_sig.params.push(AbiParam::new(I64));
        for name in [
            "ar_rt_waker_wake",
            "ar_rt_waker_destroy",
            "ar_rt_supervisor_destroy",
            "ar_rt_tcp_close",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &void_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut ret_sig = Signature::new(default_call_conv);
        ret_sig.params.push(AbiParam::new(I64));
        ret_sig.returns.push(AbiParam::new(I64));
        for name in ["ar_rt_tcp_listen", "ar_rt_tcp_accept", "ar_rt_tcp_connect"] {
            let id = module
                .declare_function(name, Linkage::Import, &ret_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut rw_sig = Signature::new(default_call_conv);
        rw_sig.params.push(AbiParam::new(I64));
        rw_sig.params.push(AbiParam::new(ptr_type));
        rw_sig.params.push(AbiParam::new(I64));
        rw_sig.returns.push(AbiParam::new(I64));
        for name in [
            "ar_rt_tcp_read",
            "ar_rt_tcp_write",
            "ar_rt_tcp_read_async",
            "ar_rt_tcp_write_async",
        ] {
            let id = module
                .declare_function(name, Linkage::Import, &rw_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }

        let mut two = Signature::new(default_call_conv);
        two.params.push(AbiParam::new(I64));
        two.params.push(AbiParam::new(I64));
        two.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_set_nonblocking", Linkage::Import, &two)
            .map_err(|err| {
                codegen_ice(format!(
                    "failed to declare ar_rt_tcp_set_nonblocking: {err:?}"
                ))
            })?;
        insert_sym(func_ids, "ar_rt_tcp_set_nonblocking", id);

        let mut three = Signature::new(default_call_conv);
        for _ in 0..3 {
            three.params.push(AbiParam::new(I64));
        }
        three.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_wait", Linkage::Import, &three)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_tcp_wait: {err:?}")))?;
        insert_sym(func_ids, "ar_rt_tcp_wait", id);

        let mut four = Signature::new(default_call_conv);
        for _ in 0..4 {
            four.params.push(AbiParam::new(I64));
        }
        four.returns.push(AbiParam::new(I64));
        let id = module
            .declare_function("ar_rt_tcp_wait_wake", Linkage::Import, &four)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_rt_tcp_wait_wake: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_rt_tcp_wait_wake", id);

        let mut sup_sig = Signature::new(default_call_conv);
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.params.push(AbiParam::new(ptr_type));
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.params.push(AbiParam::new(I64));
        sup_sig.returns.push(AbiParam::new(I64));
        for name in ["ar_rt_supervisor_spawn", "ar_rt_supervisor_spawn_str"] {
            let id = module
                .declare_function(name, Linkage::Import, &sup_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            insert_sym(func_ids, name, id);
        }
    }
    Ok(())
}

pub(super) fn declare_net_imports(
    module: &mut super::RuntimeImports,
    func_ids: &mut FxHashMap<String, usize>,
    default_call_conv: CallConv,
    ptr_type: Type,
) -> Result<(), Diagnostic> {
    // UDP and DNS
    {
        let mut bind_sig = Signature::new(default_call_conv);
        bind_sig.params.push(AbiParam::new(ptr_type));
        bind_sig.params.push(AbiParam::new(ptr_type));
        bind_sig.params.push(AbiParam::new(I32));
        bind_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_net_udp_bind", Linkage::Import, &bind_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_net_udp_bind: {err:?}")))?;
        insert_sym(func_ids, "ar_net_udp_bind", id);

        let mut sendto_sig = Signature::new(default_call_conv);
        sendto_sig.params.push(AbiParam::new(I32));
        sendto_sig.params.push(AbiParam::new(ptr_type));
        sendto_sig.params.push(AbiParam::new(ptr_type));
        sendto_sig.params.push(AbiParam::new(ptr_type));
        sendto_sig.params.push(AbiParam::new(ptr_type));
        sendto_sig.params.push(AbiParam::new(I32));
        sendto_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_net_udp_send_to", Linkage::Import, &sendto_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_net_udp_send_to: {err:?}")))?;
        insert_sym(func_ids, "ar_net_udp_send_to", id);

        let mut recvfrom_sig = Signature::new(default_call_conv);
        recvfrom_sig.params.push(AbiParam::new(I32));
        recvfrom_sig.params.push(AbiParam::new(ptr_type));
        recvfrom_sig.params.push(AbiParam::new(ptr_type));
        recvfrom_sig.params.push(AbiParam::new(ptr_type));
        recvfrom_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_net_udp_recv_from", Linkage::Import, &recvfrom_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_net_udp_recv_from: {err:?}"))
            })?;
        insert_sym(func_ids, "ar_net_udp_recv_from", id);

        let mut close_sig = Signature::new(default_call_conv);
        close_sig.params.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_net_udp_close", Linkage::Import, &close_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_net_udp_close: {err:?}")))?;
        insert_sym(func_ids, "ar_net_udp_close", id);

        let mut dns_sig = Signature::new(default_call_conv);
        dns_sig.params.push(AbiParam::new(ptr_type));
        dns_sig.params.push(AbiParam::new(ptr_type));
        dns_sig.params.push(AbiParam::new(ptr_type));
        dns_sig.params.push(AbiParam::new(ptr_type));
        dns_sig.returns.push(AbiParam::new(I32));
        let id = module
            .declare_function("ar_net_dns_resolve", Linkage::Import, &dns_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_net_dns_resolve: {err:?}")))?;
        insert_sym(func_ids, "ar_net_dns_resolve", id);
    }
    Ok(())
}
