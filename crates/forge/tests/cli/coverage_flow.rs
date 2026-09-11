use serde_json::Value;

forgetest!(json_and_lcov_reports_do_not_overwrite_each_other, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        "contract Target { function value() external pure returns (uint256) { return 42; } }",
    );
    prj.add_test(
        "Target.t.sol",
        r#"
import {Target} from "../src/Target.sol";
contract TargetTest {
    function testValue() external { require(new Target().value() == 42); }
}
"#,
    );
    cmd.args(["coverage", "--report=json", "--report=lcov", "--report-file=combined.out"])
        .assert_success();
    let json: Value = serde_json::from_str(
        &std::fs::read_to_string(prj.root().join("coverage-final.json")).unwrap(),
    )
    .unwrap();
    assert!(json["src/Target.sol"].is_object());
    let lcov = std::fs::read_to_string(prj.root().join("lcov.info")).unwrap();
    assert!(lcov.lines().any(|line| line == "SF:src/Target.sol"));
    assert!(!prj.root().join("combined.out").exists());
});

forgetest!(instrumented_attribution_tracks_each_test, |prj, cmd| {
    prj.add_source(
        "Target.sol",
        r#"
contract Target {
    uint256 public value;
    function foo() external { value = 1; }
    function bar() external { value = 2; }
}
"#,
    );
    prj.add_test(
        "Target.t.sol",
        r#"
import {Target} from "../src/Target.sol";
contract TargetTest {
    Target target = new Target();
    function testFoo() external { target.foo(); }
    function testBar() external { target.bar(); }
}
"#,
    );
    cmd.args(["coverage", "--instrumented", "--exclude-tests", "--report=attribution"])
        .assert_success();
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(prj.root().join("coverage-attribution.json")).unwrap(),
    )
    .unwrap();
    let tests = report["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2);
    for test in tests {
        let functions = test["covered"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["function"].as_str())
            .collect::<Vec<_>>();
        match test["test"].as_str().unwrap() {
            "testFoo()" => assert_eq!(functions, ["foo"]),
            "testBar()" => assert_eq!(functions, ["bar"]),
            name => panic!("unexpected test {name}"),
        }
    }
});

forgetest!(instrumented_preserves_expected_revert_return_data, |prj, cmd| {
    prj.add_test(
        "Returndata.t.sol",
        r#"
interface Vm { function expectRevert() external; }
contract Reverter {
    function fail() external pure { revert("failure"); }
}
contract ReturndataTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    Reverter target = new Reverter();
    function testExpectedRevertReturndata() external {
        vm.expectRevert();
        target.fail();
        uint256 size;
        assembly { size := returndatasize() }
        require(size == 8192, "probe changed the expected-revert result");
    }
}
"#,
    );
    cmd.arg("test").assert_success();
    cmd.forge_fuse().args(["coverage", "--instrumented"]).assert_success();
});
