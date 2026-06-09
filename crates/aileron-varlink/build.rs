fn main() {
    varlink_generator::cargo_build("varlink/aileron.Inference.varlink");
    varlink_generator::cargo_build("varlink/aileron.Models.varlink");
    varlink_generator::cargo_build("varlink/aileron.Permissions.varlink");
    varlink_generator::cargo_build("varlink/aileron.Sessions.varlink");
}
